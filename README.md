# audrey2

A small, scriptable feed aggregator that keeps every entry in a local SQLite database you can query, tag, and pipe into other tools. RSS, Atom, and JSON Feed in; tab-separated values, JSON, or plain text out.

audrey2 is built for people who already live on the command line. There's no TUI, no daemon, and no config file — just a CLI and a database. Subscribe to feeds, sync them when you want new entries, and use a small query language to find things and tag them.

## Why

Most feed readers want to be the place you read. audrey2 doesn't. It's a stable local cache of your subscriptions that plays nicely with `grep`, `xargs`, `fzf`, cron, and shell scripts. If you'd rather glue together your own reading workflow than adopt someone else's, this is the shape of tool you want.

## Install

audrey2 is a Rust project. With a Rust toolchain installed:

```
cargo install audrey2
```

Or from a local checkout:

```
cargo install --path .
```

This produces a single binary, `audrey2`, on your `$PATH`.

The database lives at `$XDG_DATA_HOME/audrey2/audrey2.db` on Linux (typically `~/.local/share/audrey2/audrey2.db`), `~/Library/Application Support/audrey2/audrey2.db` on macOS, and the equivalent under `%APPDATA%` on Windows. It's a plain SQLite file — back it up, sync it across machines, or open it with `sqlite3` whenever you like.

## Quick start

```
$ audrey2 add https://lwn.net/headlines/rss
added: lwn-net-headlines    LWN.net Headlines    (15 entries)

$ audrey2 sync
lwn-net-headlines: 0 new

$ audrey2 search tag:unread feed:lwn after:1w
4823    2026-05-08    lwn-net-headlines    Some headline here
4822    2026-05-07    lwn-net-headlines    Another headline
...

$ audrey2 show 4823
# Some headline here
https://lwn.net/Articles/...

(article body, HTML stripped)
```

## Commands

Every command takes a slug, a URL, or a unique slug prefix wherever a feed identifier is expected, so once you've added `lwn-net-headlines` you can just type `lwn`.

### `add URL [--as SLUG]`

Subscribes to a feed and immediately fetches its current entries. The slug is generated from the feed's title (lowercased, non-alphanumeric collapsed to dashes) unless you pass `--as`. If the auto-generated slug collides with an existing one, audrey2 appends `-2`, `-3`, etc.

```
audrey2 add https://example.com/feed.xml --as example
```

### `rm SLUG`

Unsubscribes and deletes every entry and tag for that feed. There is no undo.

### `ls [--json]`

Lists subscribed feeds. Default output is TSV (slug, title, url, last sync date). With `--json`, prints a pretty-printed JSON array of the same fields, with `last_sync` as a Unix timestamp.

### `sync [SLUG]`

Fetches new entries for one feed (if a slug is given) or every feed (if not). For each feed it prints `slug: N new`; failures are reported on stderr and don't stop the rest of the run, which makes this safe to put in cron.

Newly inserted entries are automatically tagged `unread`, plus a slugified version of every category the feed declares for that entry.

### `rename OLD NEW`

Renames a feed's slug. Updates entries to point at the new slug too.

### `search [TOKENS...] [--json] [--cols id,date,feed,title,url]`

Searches your entries. Tokens are AND'd together and come in five flavors:

| Token        | Matches                                                |
|--------------|--------------------------------------------------------|
| `tag:NAME`   | Entries with that tag                                  |
| `feed:SLUG`  | Entries from that feed                                 |
| `title:TEXT` | Substring match on the title                           |
| `after:DUR`  | Published within the last DUR (e.g. `after:1w`)        |
| `before:DUR` | Published more than DUR ago                            |
| `WORD`       | Substring match on title OR summary                    |

Durations are `NUMBER` followed by `h` (hour), `d` (day), `w` (week), `m` (30 days), or `y` (365 days). An empty query returns everything. Results are newest-first and capped at 200.

Default columns are `id,date,feed,title`. Pick your own with `-c`:

```
audrey2 search -c id,url tag:starred
audrey2 search -c date,feed,title rust after:3d
```

`--json` ignores `--cols` and emits the full record including tags.

### `show ID [--html] [--open]`

Prints one entry to stdout (the body is HTML-stripped by default; `--html` keeps it raw) and removes its `unread` tag. `--open` opens the entry's URL in your browser instead and leaves the tag alone.

### `tag [+TAG | -TAG ...] [QUERY...]`

Bulk-edits tags on entries matching a query. Operations come first, then query tokens. `+name` adds a tag, `-name` removes it. You can mix as many as you like in one invocation.

```
audrey2 tag +starred title:"release notes"
audrey2 tag -unread feed:lwn before:2w
audrey2 tag +read -unread tag:unread before:1m
```

### `gc DURATION`

Deletes every entry (and its tags) older than the given duration. There's no soft-delete.

```
audrey2 gc 6m   # delete entries older than 6 months
```

## A few patterns

Mark everything in a feed as read:

```
audrey2 tag -unread feed:lwn tag:unread
```

Open the newest unread thing in your browser:

```
audrey2 show "$(audrey2 search tag:unread -c id | head -1)" --open
```

Daily digest piped to a file:

```
audrey2 sync && audrey2 search after:1d > ~/today.tsv
```

Star anything from one feed that mentions a keyword:

```
audrey2 tag +starred feed:hn rust
```

Cron the sync, with errors going to a log:

```
0 * * * * audrey2 sync >> ~/.cache/audrey2-sync.log 2>> ~/.cache/audrey2-sync.err
```

## Notes and limitations

- Fetching is synchronous and one feed at a time. For most personal subscription lists this is fine; if you have hundreds of feeds, expect `audrey2 sync` to take a while.
- HTTP is plain — no conditional GET, no ETag, no If-Modified-Since. Every sync re-downloads each feed in full. Duplicate entries are skipped at insert time via `(feed_slug, guid)`, so this is correct, just not bandwidth-optimal.
- Search uses `LIKE` on the entries table. It's fine for tens of thousands of entries; it's not a full-text engine.
- Duration math treats months as 30 days and years as 365 days. Good enough for "stuff older than 6m"; not good enough for accounting.
- The duration unit `m` means *months*, not minutes. There is no minute unit.
- Show only renders one body. If a feed gives both `summary` and `content`, audrey2 prefers `content`.
- The database schema is small and stable; if you want to do something audrey2 doesn't (custom reports, exporting to OPML, syncing to a remote), it's straightforward to query the SQLite file directly.

## Schema

For when you want to bypass the CLI:

```sql
feeds   (slug PK, url UNIQUE, title, last_sync)
entries (id PK, feed_slug, guid, title, url, summary, content, published,
         UNIQUE(feed_slug, guid))
tags    (entry_id, tag, PK(entry_id, tag))
```

Indexes on `entries.published` and `tags.tag`. Timestamps are Unix seconds.
