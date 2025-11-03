# mbox2db

A fast, simple Rust-based tool to convert large mbox email archives into optimized SQLite databases. Built for handling gigabyte-sized Gmail exports with maximum performance.

## Features

- **Lightning Fast**: Single-transaction writes with optimized SQLite settings (WAL mode, memory mapping, large cache)
- **Smart Filtering**: Automatically excludes Spam and Trash by default (configurable)
- **Auto-Incrementing Filenames**: Creates dated databases (e.g., `2025-11-03-emails.db`) that auto-increment to avoid overwriting
- **Robust Date Parsing**: Handles 20+ malformed date formats commonly found in email archives
- **Progress Indicator**: Modern spinner shows real-time progress and skipped email counts
- **Full-Text Search Ready**: Creates indexes on common fields for instant queries

## Installation

```bash
# Build release binary
cargo build --release

# Binary will be at ./target/release/mbox2db
```

## Quick Start

```bash
# Convert mbox to SQLite (excludes Spam/Trash by default)
./target/release/mbox2db all-mail.mbox

# Output: exports/2025-11-03-emails.db
```

## Usage

```
mbox2db [OPTIONS] <INPUT>

Arguments:
  <INPUT>  Input mbox file path

Options:
  -o, --output <OUTPUT>              Custom output database path
  -d, --destructive                  Overwrite existing database instead of auto-incrementing
      --include-spam                 Include emails marked as Spam
      --include-trash                Include emails marked as Trash
      --include-spam-and-trash       Include both Spam and Trash emails
  -h, --help                         Print help
```

## Examples

### Basic Conversion (Default Behavior)

```bash
# Filters out Spam/Trash, creates dated output file
./target/release/mbox2db all-mail.mbox
# Output: exports/2025-11-03-emails.db

# Running again on the same day creates incremented file
./target/release/mbox2db all-mail.mbox
# Output: exports/2025-11-03-emails-0001.db
```

### Include Spam/Trash

```bash
# Include spam emails only
./target/release/mbox2db all-mail.mbox --include-spam

# Include trash emails only
./target/release/mbox2db all-mail.mbox --include-trash

# Include both spam and trash
./target/release/mbox2db all-mail.mbox --include-spam-and-trash
```

### Custom Output Path

```bash
# Specify custom output location
./target/release/mbox2db all-mail.mbox -o ~/Documents/my-emails.db

# Overwrite existing file (destructive mode)
./target/release/mbox2db all-mail.mbox -d -o exports/emails.db
```

## Database Schema

```sql
CREATE TABLE emails (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_addr TEXT,
    to_addr TEXT,
    cc TEXT,
    bcc TEXT,
    subject TEXT,
    date TEXT,              -- Original email date header
    date_parsed TEXT,       -- Parsed datetime in SQLite format (YYYY-MM-DD HH:MM:SS)
    message_id TEXT,
    in_reply_to TEXT,
    refs TEXT,              -- "references" header
    content_type TEXT,
    body_plain TEXT,
    body_html TEXT
);

-- Indexes for fast queries
CREATE INDEX idx_from ON emails(from_addr);
CREATE INDEX idx_date ON emails(date);
CREATE INDEX idx_date_parsed ON emails(date_parsed);
CREATE INDEX idx_subject ON emails(subject);
```

## Querying Your Database

### Basic Queries

```sql
-- Count all emails
SELECT COUNT(*) FROM emails;

-- Count emails from specific sender
SELECT COUNT(*) FROM emails WHERE from_addr LIKE '%user@example.com%';

-- Get most recent emails
SELECT subject, from_addr, date_parsed 
FROM emails 
ORDER BY date_parsed DESC 
LIMIT 10;
```

### Search by Date

```sql
-- Get emails from 2025
SELECT * FROM emails 
WHERE date_parsed LIKE '2025%'
ORDER BY date_parsed DESC;

-- Count emails by year
SELECT strftime('%Y', date_parsed) as year, COUNT(*) 
FROM emails 
WHERE date_parsed IS NOT NULL
GROUP BY year 
ORDER BY year;

-- Get emails from date range
SELECT subject, date_parsed, from_addr 
FROM emails 
WHERE date_parsed BETWEEN '2020-01-01' AND '2020-12-31'
ORDER BY date_parsed DESC;
```

### Full-Text Search

```sql
-- Search subject lines
SELECT subject, date_parsed, from_addr 
FROM emails 
WHERE subject LIKE '%keyword%'
ORDER BY date_parsed DESC;

-- Search email body
SELECT subject, from_addr, date_parsed 
FROM emails 
WHERE body_plain LIKE '%search term%' 
   OR body_html LIKE '%search term%'
ORDER BY date_parsed DESC;
```

### Email Threads

```sql
-- Find email threads by message_id/in_reply_to
SELECT * FROM emails 
WHERE in_reply_to = '<some-message-id>'
ORDER BY date_parsed;
```

## Performance Notes

- **Optimized SQLite Settings**:
  - WAL (Write-Ahead Logging) mode for better concurrency
  - NORMAL synchronous mode for fast writes
  - 64MB cache size
  - 30GB memory mapping
  - Single transaction for all inserts (~10-100x faster)
  
- **Handles Large Files**: Tested with multi-GB mbox files containing 80,000+ emails

- **Date Parsing**: Handles malformed dates including:
  - Double-dash timezones (`--0400`)
  - Single-digit time components (`9:47:11`)
  - Two-digit years (`Jun 09`)
  - Named timezones (`Eastern Daylight Time`, `GMT-0700`)
  - Various date formats (`7/19/2005 8:11:52 AM`)

## How to Export Gmail to mbox

1. Go to [Google Takeout](https://takeout.google.com/)
2. Deselect all products, then select **Mail**
3. Click "All Mail data included" and select specific labels if desired
4. Choose "Export once" and "Send download link via email"
5. Select file format: `.zip` or `.tgz`
6. Click "Create export"
7. Download and extract the `All mail Including Spam and Trash.mbox` file

## License

MIT

## Author

Eric Hamiter
