# audrey2

A command-line feed aggregator that stores entries in a local SQLite database. Reads RSS, Atom, and JSON Feed. Outputs TSV, JSON, or plain text.

> *"Feed me Seymour, feed me now!"*

## Install

```
cargo install audrey2
```

Or from a local checkout:

```
cargo install --path .
```

The database is at `$XDG_DATA_HOME/audrey2/audrey2.db` on Linux (typically `~/.local/share/audrey2/audrey2.db`), `~/Library/Application Support/audrey2/audrey2.db` on macOS, and the equivalent under `%APPDATA%` on Windows.

## Quick start

```
$ audrey2 add https://lwn.net/headlines/rss
added: lwn-net-headlines    LWN.net Headlines    (15 entries)

$ audrey2 sync
lwn-net-headlines: 0 new

$ audrey2 search tag:unread feed:lwn after:1w
4823    2026-05-08    lwn-net-headlines    Some headline here
4822    2026-05-07    lwn-net-headlines    Another headline

$ audrey2 show 4823
# Some headline here
https://lwn.net/Articles/...

(article body, HTML stripped)
```

## Commands

A feed identifier may be a slug, a URL, or a unique slug prefix.

### `add URL [--as SLUG]`

Subscribes to a feed and fetches its current entries. The slug is generated from the feed's title (lowercased, non-alphanumeric collapsed to dashes) unless `--as` is given. Collisions are resolved by appending `-2`, `-3`, etc.

### `rm SLUG`

Unsubscribes and deletes all entries and tags for the feed. Not reversible.

### `ls [--json]`

Lists subscribed feeds. Default output is TSV (slug, title, url, last sync date). `--json` prints the same fields as JSON, with `last_sync` as a Unix timestamp.

### `sync [SLUG]`

Fetches new entries for one feed, or all feeds if no slug is given. Prints `slug: N new` per feed; failures go to stderr and do not abort the run.

New entries are tagged `unread` plus a slugified copy of every category declared in the feed.

### `rename OLD NEW`

Renames a feed's slug and updates its entries.

### `search [TOKENS...] [--json] [--cols id,date,feed,title,url]`

Searches entries. Tokens are AND'd:

| Token        | Matches                                          |
|--------------|--------------------------------------------------|
| `tag:NAME`   | Entries with that tag                            |
| `feed:SLUG`  | Entries from that feed                           |
| `title:TEXT` | Substring match on the title                     |
| `after:DUR`  | Published within the last DUR (e.g. `after:1w`)  |
| `before:DUR` | Published more than DUR ago                      |
| `WORD`       | Substring match on title OR summary              |

Durations are `NUMBER` followed by `h`, `d`, `w`, `m` (30 days), or `y` (365 days). An empty query returns everything. Results are newest-first, capped at 200.

Default columns are `id,date,feed,title`. Override with `-c`:

```
audrey2 search -c id,url tag:starred
audrey2 search -c date,feed,title rust after:3d
```

`--json` ignores `--cols` and emits the full record including tags.

### `show ID [--html] [--open]`

Prints one entry to stdout and removes its `unread` tag. Body is HTML-stripped by default; `--html` keeps it raw. `--open` opens the entry's URL in a browser instead and leaves the tag.

### `tag [+TAG | -TAG ...] QUERY...`

Adds or removes tags on entries matching a query. Operations come first, then query tokens. `+name` adds, `-name` removes. Multiple operations may be combined.

At least one query token is required. Running `tag` with only operations and no query is refused, to prevent accidentally tagging every entry. To genuinely apply a tag to all entries, use a query that matches everything explicitly, e.g. `before:100y`.

```
audrey2 tag +starred title:"release notes"
audrey2 tag -unread feed:lwn before:2w
audrey2 tag +read -unread tag:unread before:1m
```

### `tags`

Lists every tag with its entry count. Output is TSV (count, tag), most-used first.

```
$ audrey2 tags
1247    unread
89      rust
54      kernel
12      starred
```

### `gc DURATION`

Deletes entries (and their tags) older than the given duration.

```
audrey2 gc 6m
```

### `export-markdown DIR [--prune]`

Writes one markdown note per entry to `DIR/<feed_slug>/<id>.md` with YAML frontmatter (`audrey_id`, `feed`, `published`, `tags`, `title`, `url`) and the entry body. Filenames use the entry id, which is stable. Files are only rewritten if their content has changed.

`--prune` deletes notes under `DIR` whose entry ids are no longer in the database.

```
audrey2 export-markdown ~/notes/audrey
audrey2 export-markdown ~/notes/audrey --prune
```

## Examples

Mark everything in a feed as read:

```
audrey2 tag -unread feed:lwn tag:unread
```

Open the newest unread entry in a browser:

```
audrey2 show "$(audrey2 search tag:unread -c id | head -1)" --open
```

Daily digest to a file:

```
audrey2 sync && audrey2 search after:1d > ~/today.tsv
```

Tag entries from one feed that match a keyword:

```
audrey2 tag +starred feed:hn rust
```

Cron sync with logging:

```
0 * * * * audrey2 sync >> ~/.cache/audrey2-sync.log 2>> ~/.cache/audrey2-sync.err
```

## Notes and limitations

- Sync is sequential and synchronous.
- HTTP fetches do not use conditional GET, ETag, or If-Modified-Since. Each sync re-downloads each feed in full. Duplicate entries are skipped at insert via `(feed_slug, guid)`.
- Search uses SQL `LIKE`. Not a full-text engine.
- Duration math treats months as 30 days and years as 365 days.
- The duration unit `m` is months, not minutes. There is no minute unit.
- `show` prefers `content` over `summary` when both are present.

## Schema

```sql
feeds   (slug PK, url UNIQUE, title, last_sync)
entries (id PK, feed_slug, guid, title, url, summary, content, published,
         UNIQUE(feed_slug, guid))
tags    (entry_id, tag, PK(entry_id, tag))
```

Indexes on `entries.published` and `tags.tag`. Timestamps are Unix seconds.
