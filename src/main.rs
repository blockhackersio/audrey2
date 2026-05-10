use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use feed_rs::parser;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS feeds (slug TEXT PRIMARY KEY, url TEXT UNIQUE NOT NULL, title TEXT, last_sync INTEGER);
CREATE TABLE IF NOT EXISTS entries (id INTEGER PRIMARY KEY, feed_slug TEXT NOT NULL, guid TEXT, title TEXT, url TEXT, summary TEXT, content TEXT, published INTEGER, UNIQUE(feed_slug, guid));
CREATE TABLE IF NOT EXISTS tags (entry_id INTEGER NOT NULL, tag TEXT NOT NULL, PRIMARY KEY (entry_id, tag));
CREATE INDEX IF NOT EXISTS idx_published ON entries(published);
CREATE INDEX IF NOT EXISTS idx_tag ON tags(tag);
";

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Col {
    Id,
    Date,
    Feed,
    Title,
    Url,
}

#[derive(Parser)]
#[command(
    name = "audrey2",
    version,
    about = "Feed aggregator with a queryable local store"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Subscribe to a feed by URL
    Add {
        /// Feed URL (RSS, Atom, JSON Feed)
        url: String,
        /// Override the auto-generated slug
        #[arg(long = "as")]
        slug: Option<String>,
    },
    /// Unsubscribe and delete all entries for a feed
    Rm {
        /// Slug, URL, or unique slug prefix
        slug: String,
    },
    /// List subscribed feeds (TSV: slug, title, url, last_sync)
    Ls {
        /// Output JSON instead of TSV
        #[arg(long)]
        json: bool,
    },
    /// Fetch new entries; all feeds if no slug given
    Sync {
        /// Slug, URL, or unique slug prefix; omit for all
        slug: Option<String>,
    },
    /// Rename a feed's slug
    Rename {
        /// Existing slug (or unique prefix)
        old: String,
        /// New slug
        new: String,
    },
    /// Search entries (TSV: id, date, feed, title)
    ///
    /// Query tokens (AND'd): tag:NAME, feed:SLUG, title:TEXT,
    /// after:DUR, before:DUR (DUR = 2w, 3d, 12h, 1m, 1y),
    /// or bare words matching title/summary.
    /// Empty query returns everything (newest first, capped at 200).
    Search {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        query: Vec<String>,
        #[arg(long, short = 'j')]
        json: bool,
        /// Columns to print, comma-separated (id,date,feed,title,url)
        #[arg(long, short = 'c', value_delimiter = ',', default_values = ["id", "date", "feed", "title"])]
        cols: Vec<Col>,
    },
    /// Print one entry to stdout and clear its 'unread' tag
    Show {
        /// Entry id (from `audrey2 search`)
        id: i64,
        /// Keep raw HTML instead of stripping tags
        #[arg(long)]
        html: bool,
        /// Open the entry's URL in your browser
        #[arg(long)]
        open: bool,
    },
    /// Add or remove tags on entries matching a query
    ///
    /// Args: one or more +TAG / -TAG ops, then query tokens.
    /// At least one query token is required — bare `tag +foo`
    /// (no query) is refused to prevent tagging every entry.
    /// Example: audrey2 tag +starred -unread tag:rust feed:lwn
    Tag {
        /// +TAG to add, -TAG to remove, then query tokens
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List all tags with counts
    Tags,
    /// Delete entries older than a duration (e.g. 2w, 6m, 1y)
    Gc {
        /// Duration: NUMBER + h/d/w/m/y
        duration: String,
    },
    /// Export selected entries to a directory of markdown notes
    ///
    /// Writes <DIR>/<feed>/<id>.md per id with YAML frontmatter.
    /// Files are only rewritten if their content has changed.
    /// Clears the 'unread' tag on each exported entry.
    ExportMarkdown {
        /// Output directory (created if missing)
        dir: std::path::PathBuf,
        /// Entry ids to export (from `audrey2 search`)
        #[arg(required = true)]
        ids: Vec<i64>,
    },
}

fn db() -> Result<Connection> {
    let dir = dirs::data_dir()
        .ok_or_else(|| anyhow!("no data dir"))?
        .join("audrey2");
    std::fs::create_dir_all(&dir)?;
    let c = Connection::open(dir.join("audrey2.db"))?;
    c.execute_batch(SCHEMA)?;
    Ok(c)
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = true;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let r = out.trim_matches('-').to_string();
    if r.is_empty() {
        "feed".into()
    } else {
        r
    }
}

fn unique_slug(c: &Connection, base: &str) -> Result<String> {
    let mut s = base.to_string();
    let mut n = 2;
    while c
        .query_row("SELECT 1 FROM feeds WHERE slug = ?", [&s], |_| Ok(()))
        .optional()?
        .is_some()
    {
        s = format!("{}-{}", base, n);
        n += 1;
    }
    Ok(s)
}

fn resolve(c: &Connection, p: &str) -> Result<String> {
    if c.query_row("SELECT 1 FROM feeds WHERE slug = ?", [p], |_| Ok(()))
        .optional()?
        .is_some()
    {
        return Ok(p.into());
    }
    if let Some(s) = c
        .query_row("SELECT slug FROM feeds WHERE url = ?", [p], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    {
        return Ok(s);
    }
    let mut stmt = c.prepare("SELECT slug FROM feeds WHERE slug LIKE ?")?;
    let m: Vec<String> = stmt
        .query_map([format!("{}%", p)], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    match m.len() {
        1 => Ok(m.into_iter().next().unwrap()),
        0 => bail!("no feed matching '{}'", p),
        _ => bail!("ambiguous '{}': {}", p, m.join(", ")),
    }
}

fn parse_dur(s: &str) -> Result<i64> {
    if s.is_empty() {
        bail!("empty duration");
    }
    let (n, u) = s.split_at(s.len() - 1);
    let n: i64 = n.parse().context("bad duration number")?;
    let mul = match u {
        "h" => 3600,
        "d" => 86400,
        "w" => 604800,
        "m" => 2592000,
        "y" => 31536000,
        _ => bail!("bad duration unit, use h/d/w/m/y"),
    };
    Ok(n * mul)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn fetch(url: &str) -> Result<feed_rs::model::Feed> {
    let bytes = reqwest::blocking::Client::builder()
        .user_agent("audrey2/0.1")
        .build()?
        .get(url)
        .send()?
        .error_for_status()?
        .bytes()?;
    Ok(parser::parse(&bytes[..])?)
}

fn ingest(c: &Connection, slug: &str, f: &feed_rs::model::Feed) -> Result<usize> {
    let mut n = 0;
    for e in &f.entries {
        let title = e
            .title
            .as_ref()
            .map(|t| t.content.clone())
            .unwrap_or_default();
        let url = e.links.first().map(|l| l.href.clone()).unwrap_or_default();
        let summary = e
            .summary
            .as_ref()
            .map(|s| s.content.clone())
            .unwrap_or_default();
        let content = e
            .content
            .as_ref()
            .and_then(|c| c.body.clone())
            .unwrap_or_default();
        let published = e
            .published
            .or(e.updated)
            .map(|d| d.timestamp())
            .unwrap_or_else(now);
        let r = c.execute(
            "INSERT OR IGNORE INTO entries (feed_slug, guid, title, url, summary, content, published) VALUES (?,?,?,?,?,?,?)",
            params![slug, e.id, title, url, summary, content, published],
        )?;
        if r > 0 {
            let id = c.last_insert_rowid();
            c.execute("INSERT OR IGNORE INTO tags VALUES (?, 'unread')", [id])?;
            for cat in &e.categories {
                c.execute(
                    "INSERT OR IGNORE INTO tags VALUES (?, ?)",
                    params![id, slugify(&cat.term)],
                )?;
            }
            n += 1;
        }
    }
    c.execute(
        "UPDATE feeds SET last_sync = ? WHERE slug = ?",
        params![now(), slug],
    )?;
    Ok(n)
}

fn build_query(tokens: &[String]) -> Result<(String, Vec<String>)> {
    let mut wh: Vec<String> = vec![];
    let mut ps: Vec<String> = vec![];
    for t in tokens {
        if let Some(v) = t.strip_prefix("tag:") {
            wh.push("e.id IN (SELECT entry_id FROM tags WHERE tag = ?)".into());
            ps.push(v.into());
        } else if let Some(v) = t.strip_prefix("feed:") {
            wh.push("e.feed_slug = ?".into());
            ps.push(v.into());
        } else if let Some(v) = t.strip_prefix("after:") {
            wh.push("e.published >= ?".into());
            ps.push((now() - parse_dur(v)?).to_string());
        } else if let Some(v) = t.strip_prefix("before:") {
            wh.push("e.published <= ?".into());
            ps.push((now() - parse_dur(v)?).to_string());
        } else if let Some(v) = t.strip_prefix("title:") {
            wh.push("e.title LIKE ?".into());
            ps.push(format!("%{}%", v));
        } else {
            wh.push("(e.title LIKE ? OR e.summary LIKE ?)".into());
            let p = format!("%{}%", t);
            ps.push(p.clone());
            ps.push(p);
        }
    }
    Ok((
        if wh.is_empty() {
            "1=1".into()
        } else {
            wh.join(" AND ")
        },
        ps,
    ))
}

fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let d = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let mut r = String::new();
    let mut blank = true;
    for line in d.lines() {
        let t = line.trim();
        if t.is_empty() {
            if !blank {
                r.push('\n');
            }
            blank = true;
        } else {
            r.push_str(t);
            r.push('\n');
            blank = false;
        }
    }
    r
}

fn open_url(u: &str) -> Result<()> {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd).arg(u).spawn()?;
    Ok(())
}

fn fmt_date(t: i64) -> String {
    if t == 0 {
        return "never".into();
    }
    chrono::DateTime::from_timestamp(t, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn render_markdown(
    id: i64,
    feed: &str,
    published: i64,
    tags: &[String],
    title: &str,
    url: &str,
    body: &str,
) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut out = String::from("---\n");
    out.push_str(&format!("audrey_id: {}\n", id));
    out.push_str(&format!("feed: \"{}\"\n", esc(feed)));
    out.push_str(&format!("published: \"{}\"\n", fmt_date(published)));
    out.push_str("tags:\n");
    for t in tags {
        out.push_str(&format!("  - {}\n", t));
    }
    out.push_str(&format!("title: \"{}\"\n", esc(title)));
    out.push_str(&format!("url: \"{}\"\n", esc(url)));
    out.push_str("---\n\n");
    out.push_str(&format!("# {}\n\n<{}>\n\n{}\n", title, url, body));
    out
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let c = db()?;
    match cli.cmd {
        Cmd::Add { url, slug } => {
            let f = fetch(&url)?;
            let title = f
                .title
                .as_ref()
                .map(|t| t.content.clone())
                .unwrap_or_else(|| url.clone());
            let base = slug.unwrap_or_else(|| slugify(&title));
            let s = unique_slug(&c, &base)?;
            c.execute(
                "INSERT INTO feeds (slug, url, title, last_sync) VALUES (?,?,?,0)",
                params![s, url, title],
            )?;
            let n = ingest(&c, &s, &f)?;
            println!("added: {}\t{}\t({} entries)", s, title, n);
        }
        Cmd::Rm { slug } => {
            let s = resolve(&c, &slug)?;
            c.execute(
                "DELETE FROM tags WHERE entry_id IN (SELECT id FROM entries WHERE feed_slug = ?)",
                [&s],
            )?;
            c.execute("DELETE FROM entries WHERE feed_slug = ?", [&s])?;
            c.execute("DELETE FROM feeds WHERE slug = ?", [&s])?;
            println!("removed: {}", s);
        }
        Cmd::Ls { json } => {
            let mut stmt =
                c.prepare("SELECT slug, title, url, last_sync FROM feeds ORDER BY slug")?;
            let rows: Vec<(String, String, String, i64)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?;
            if json {
                let v: Vec<_> = rows.iter().map(|(s, t, u, l)| serde_json::json!({"slug": s, "title": t, "url": u, "last_sync": l})).collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                for (s, t, u, l) in rows {
                    println!("{}\t{}\t{}\t{}", s, t, u, fmt_date(l));
                }
            }
        }
        Cmd::Sync { slug } => {
            let feeds: Vec<(String, String)> = match slug {
                Some(p) => {
                    let s = resolve(&c, &p)?;
                    let url: String =
                        c.query_row("SELECT url FROM feeds WHERE slug = ?", [&s], |r| r.get(0))?;
                    vec![(s, url)]
                }
                None => c
                    .prepare("SELECT slug, url FROM feeds")?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?,
            };
            for (s, u) in feeds {
                match fetch(&u) {
                    Ok(f) => println!("{}: {} new", s, ingest(&c, &s, &f)?),
                    Err(e) => eprintln!("{}: {}", s, e),
                }
            }
        }
        Cmd::Rename { old, new } => {
            let s = resolve(&c, &old)?;
            c.execute("UPDATE feeds SET slug = ? WHERE slug = ?", params![new, s])?;
            c.execute(
                "UPDATE entries SET feed_slug = ? WHERE feed_slug = ?",
                params![new, s],
            )?;
            println!("renamed {} -> {}", s, new);
        }
        Cmd::Search { query, json, cols } => {
            let (w, ps) = build_query(&query)?;
            let sql = format!(
                "SELECT e.id, e.feed_slug, e.title, e.url, e.published FROM entries e WHERE {} ORDER BY e.published DESC LIMIT 200",
                w
            );
            let mut stmt = c.prepare(&sql)?;
            let rows: Vec<(i64, String, String, String, i64)> = stmt
                .query_map(params_from_iter(ps), |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            if json {
                let mut out = vec![];
                for (i, f, t, u, p) in &rows {
                    let mut ts = c.prepare("SELECT tag FROM tags WHERE entry_id = ?")?;
                    let tags: Vec<String> = ts
                        .query_map([i], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    out.push(serde_json::json!({"id": i, "feed": f, "title": t, "url": u, "published": p, "tags": tags}));
                }
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                for (i, f, t, u, p) in rows {
                    let row: Vec<String> = cols
                        .iter()
                        .map(|c| match c {
                            Col::Id => i.to_string(),
                            Col::Date => fmt_date(p),
                            Col::Feed => f.clone(),
                            Col::Title => t.clone(),
                            Col::Url => u.clone(),
                        })
                        .collect();
                    println!("{}", row.join("\t"));
                }
            }
        }
        Cmd::Show { id, html, open } => {
            let (title, url, summary, content): (String, String, String, String) = c.query_row(
                "SELECT title, url, summary, content FROM entries WHERE id = ?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            if open {
                return open_url(&url);
            }
            let body = if !content.is_empty() {
                content
            } else {
                summary
            };
            let body = if html { body } else { strip_html(&body) };
            println!("# {}\n{}\n\n{}", title, url, body);
            c.execute(
                "DELETE FROM tags WHERE entry_id = ? AND tag = 'unread'",
                [id],
            )?;
        }
        Cmd::Tag { args } => {
            let (ops, rest): (Vec<String>, Vec<String>) = args
                .into_iter()
                .partition(|a| a.starts_with('+') || a.starts_with('-'));
            if ops.is_empty() {
                bail!("no tag ops (use +foo or -bar)");
            }
            if rest.is_empty() {
                bail!(
                    "refusing to tag every entry: provide at least one query token \
                     (e.g. tag:..., feed:..., before:..., or a search word)"
                );
            }
            let (w, ps) = build_query(&rest)?;
            let sql = format!("SELECT e.id FROM entries e WHERE {}", w);
            let mut stmt = c.prepare(&sql)?;
            let ids: Vec<i64> = stmt
                .query_map(params_from_iter(ps), |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in &ids {
                for op in &ops {
                    let (sign, tag) = op.split_at(1);
                    if sign == "+" {
                        c.execute("INSERT OR IGNORE INTO tags VALUES (?, ?)", params![id, tag])?;
                    } else {
                        c.execute(
                            "DELETE FROM tags WHERE entry_id = ? AND tag = ?",
                            params![id, tag],
                        )?;
                    }
                }
            }
            println!("{} entries updated", ids.len());
        }
        Cmd::Tags => {
            let mut stmt =
                c.prepare("SELECT tag, COUNT(*) FROM tags GROUP BY tag ORDER BY 2 DESC, tag")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for row in rows {
                let (tag, n) = row?;
                println!("{}\t{}", n, tag);
            }
        }
        Cmd::Gc { duration } => {
            let cutoff = now() - parse_dur(&duration)?;
            c.execute(
                "DELETE FROM tags WHERE entry_id IN (SELECT id FROM entries WHERE published < ?)",
                [cutoff],
            )?;
            let m = c.execute("DELETE FROM entries WHERE published < ?", [cutoff])?;
            println!("deleted {} entries", m);
        }

        Cmd::ExportMarkdown { dir, ids } => {
            std::fs::create_dir_all(&dir)?;
            let (mut wrote, mut skipped, mut missing) = (0u32, 0u32, 0u32);

            for id in ids {
                let row: Option<(String, String, String, String, String, i64)> = c
                    .query_row(
                        "SELECT feed_slug, title, url, summary, content, published \
                         FROM entries WHERE id = ?",
                        [id],
                        |r| {
                            Ok((
                                r.get(0)?,
                                r.get(1)?,
                                r.get(2)?,
                                r.get(3)?,
                                r.get(4)?,
                                r.get(5)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some((feed, title, url, summary, content, published)) = row else {
                    eprintln!("not found: {}", id);
                    missing += 1;
                    continue;
                };

                let mut tag_stmt =
                    c.prepare("SELECT tag FROM tags WHERE entry_id = ? ORDER BY tag")?;
                let tags: Vec<String> = tag_stmt
                    .query_map([id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<_>>()?;

                let feed_dir = dir.join(&feed);
                std::fs::create_dir_all(&feed_dir)?;
                let path = feed_dir.join(format!("{}.md", id));

                let body = if !content.is_empty() {
                    content
                } else {
                    summary
                };
                let new = render_markdown(id, &feed, published, &tags, &title, &url, &body);

                match std::fs::read_to_string(&path) {
                    Ok(existing) if existing == new => skipped += 1,
                    _ => {
                        std::fs::write(&path, new)?;
                        wrote += 1;
                    }
                }

                c.execute(
                    "DELETE FROM tags WHERE entry_id = ? AND tag = 'unread'",
                    [id],
                )?;
            }

            println!(
                "wrote {}, skipped {} (unchanged), missing {}",
                wrote, skipped, missing
            );
        }
    }
    Ok(())
}
