use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDateTime};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use mailparse::parse_mail;
use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{Connection, params};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "mbox2db")]
#[command(about = "Convert mbox files to SQLite database", long_about = None)]
struct Cli {
    #[arg(help = "Input mbox file path")]
    input: PathBuf,

    #[arg(short, long, help = "Output database file path (default: YYYY-MM-DD-emails.db)")]
    output: Option<PathBuf>,

    #[arg(short, long, help = "Overwrite existing database instead of auto-incrementing filename")]
    destructive: bool,

    #[arg(long, help = "Include emails marked as Spam")]
    include_spam: bool,

    #[arg(long, help = "Include emails marked as Trash")]
    include_trash: bool,

    #[arg(long, help = "Include both Spam and Trash emails")]
    include_spam_and_trash: bool,
}

#[derive(Debug)]
struct EmailRecord {
    from: String,
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    date: String,
    message_id: String,
    in_reply_to: String,
    references: String,
    content_type: String,
    body_plain: String,
    body_html: String,
    gmail_labels: String,
}

impl Default for EmailRecord {
    fn default() -> Self {
        Self {
            from: String::new(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: String::new(),
            date: String::new(),
            message_id: String::new(),
            in_reply_to: String::new(),
            references: String::new(),
            content_type: String::new(),
            body_plain: String::new(),
            body_html: String::new(),
            gmail_labels: String::new(),
        }
    }
}

fn extract_email_data(raw_email: &[u8]) -> Result<EmailRecord> {
    // Fix malformed headers: remove leading spaces from lines that shouldn't have them
    let raw_str = String::from_utf8_lossy(raw_email);
    let fixed_email = raw_str
        .lines()
        .map(|line| {
            // If line starts with space but doesn't look like a continuation (no previous header context),
            // just trim it. This is a simple heuristic.
            if line.starts_with(' ') && !line.trim_start().is_empty() {
                line.trim_start()
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    
    let parsed = parse_mail(fixed_email.as_bytes())?;
    let mut record = EmailRecord::default();

    for header in &parsed.headers {
        let name = header.get_key().to_lowercase();
        let value = header.get_value();

        match name.as_str() {
            "from" => record.from = value,
            "to" => record.to = value,
            "cc" => record.cc = value,
            "bcc" => record.bcc = value,
            "subject" => record.subject = value,
            "date" => record.date = value,
            "message-id" => record.message_id = value,
            "in-reply-to" => record.in_reply_to = value,
            "references" => record.references = value,
            "content-type" => record.content_type = value,
            "x-gmail-labels" => record.gmail_labels = value,
            _ => {}
        }
    }

    extract_body(&parsed, &mut record);

    Ok(record)
}

fn extract_body(parsed: &mailparse::ParsedMail, record: &mut EmailRecord) {
    if parsed.subparts.is_empty() {
        let content_type = parsed
            .headers
            .iter()
            .find(|h| h.get_key().to_lowercase() == "content-type")
            .map(|h| h.get_value().to_lowercase())
            .unwrap_or_default();

        if let Ok(body) = parsed.get_body() {
            if content_type.contains("text/html") {
                record.body_html = body;
            } else {
                record.body_plain = body;
            }
        }
    } else {
        for part in &parsed.subparts {
            extract_body(part, record);
        }
    }
}

fn create_database(db_path: &PathBuf) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
    }

    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to create database: {}", db_path.display()))?;

    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA cache_size=-64000;
         PRAGMA temp_store=MEMORY;
         PRAGMA mmap_size=30000000000;"
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS emails (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            from_addr TEXT,
            to_addr TEXT,
            cc TEXT,
            bcc TEXT,
            subject TEXT,
            date TEXT,
            date_parsed TEXT,
            message_id TEXT,
            in_reply_to TEXT,
            refs TEXT,
            content_type TEXT,
            body_plain TEXT,
            body_html TEXT
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_from ON emails(from_addr)",
        [],
    )?;
    
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_date ON emails(date)",
        [],
    )?;
    
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_date_parsed ON emails(date_parsed)",
        [],
    )?;
    
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_subject ON emails(subject)",
        [],
    )?;

    Ok(conn)
}

static GMT_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"GMT([+-])(\d{2}):?(\d{2})").unwrap());
static TZ_3DIGIT: Lazy<Regex> = Lazy::new(|| Regex::new(r"([+-])(\d{3})\s*$").unwrap());
static SINGLE_DIGIT_TIME: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(\d):(\d{2}):(\d{2})\b").unwrap());
static SINGLE_DIGIT_MIN_SEC: Lazy<Regex> = Lazy::new(|| Regex::new(r":(\d)\b").unwrap());

fn parse_email_date(date_str: &str) -> Option<String> {
    let mut cleaned = date_str.trim().to_string();
    
    // Skip empty dates
    if cleaned.is_empty() {
        return None;
    }
    
    // Fix: Double-dash timezone (e.g., "--0400" -> "-0400")
    cleaned = cleaned.replace("--", "-");
    
    // Fix: Strip garbage after timezone (e.g., "+0000.395-508222")
    if let Some(tz_pos) = cleaned.rfind(|c: char| c == '+' || c == '-') {
        if tz_pos > 0 && tz_pos + 5 < cleaned.len() {
            let after_tz = &cleaned[tz_pos + 5..];
            if after_tz.chars().any(|c| !c.is_whitespace()) {
                cleaned = cleaned[..tz_pos + 5].to_string();
            }
        }
    }
    
    // Fix: Strip timezone name in parentheses (e.g., "(Eastern Daylight Time)")
    if cleaned.contains('(') {
        cleaned = cleaned.split('(').next().unwrap_or(&cleaned).trim().to_string();
    }
    
    // Fix: GMT timezones with regex (GMT-07:00, GMT-0700, etc.)
    cleaned = GMT_PATTERN.replace_all(&cleaned, "$1$2$3").to_string();
    
    // Fix: Replace long timezone names and abbreviations
    cleaned = cleaned
        .replace("Eastern Daylight Time", "-0400")
        .replace("Eastern Standard Time", "-0500")
        .replace("Pacific Daylight Time", "-0700")
        .replace("Pacific Standard Time", "-0800")
        .replace("Central Daylight Time", "-0500")
        .replace("Central Standard Time", "-0600")
        .replace("Mountain Daylight Time", "-0600")
        .replace("Mountain Standard Time", "-0700")
        .replace(" UTC", " +0000")
        .replace(" GMT", " +0000")
        .replace(" EDT", " -0400")
        .replace(" EST", " -0500")
        .replace(" CDT", " -0500")
        .replace(" CST", " -0600")
        .replace(" PDT", " -0700")
        .replace(" PST", " -0800")
        .replace(" CET", " +0100");
    
    // Fix: 3-digit timezone without leading zero (e.g., "-600" -> "-0600")
    cleaned = TZ_3DIGIT.replace_all(&cleaned, "${1}0$2").to_string();
    
    // Fix: Single-digit hour (e.g., "9:47:11" -> "09:47:11")
    cleaned = SINGLE_DIGIT_TIME.replace_all(&cleaned, "0$1:$2:$3").to_string();
    
    // Fix: Single-digit minute/second (e.g., "21:9:7" -> "21:09:07")
    cleaned = SINGLE_DIGIT_MIN_SEC.replace_all(&cleaned, ":0$1").to_string();
    
    // Fix: PM/AM with timezone (e.g., "PM+0400" or "PM CDT")
    cleaned = cleaned.replace("PM+", " +").replace("PM-", " -").replace("AM+", " +").replace("AM-", " -").replace(" PM ", " ").replace(" AM ", " ");
    
    // Fix: Full day names (e.g., "Thursday" -> "Thu", "Thurs" -> "Thu")
    cleaned = cleaned
        .replace("Monday", "Mon")
        .replace("Tuesday", "Tue")
        .replace("Wednesday", "Wed")
        .replace("Thursday", "Thu")
        .replace("Thurs,", "Thu,")
        .replace("Friday", "Fri")
        .replace("Saturday", "Sat")
        .replace("Sunday", "Sun");
    
    // Fix: Full month names (e.g., "March" -> "Mar")
    cleaned = cleaned
        .replace("January", "Jan")
        .replace("February", "Feb")
        .replace("March", "Mar")
        .replace("April", "Apr")
        .replace("June", "Jun")
        .replace("July", "Jul")
        .replace("August", "Aug")
        .replace("September", "Sep")
        .replace("October", "Oct")
        .replace("November", "Nov")
        .replace("December", "Dec");
    
    // Try standard RFC2822
    if let Ok(dt) = DateTime::parse_from_rfc2822(&cleaned) {
        return Some(dt.format("%Y-%m-%d %H:%M:%S").to_string());
    }
    
    // Fix: Missing comma after day-of-week (e.g., "Tue 02 Mar" -> "Tue, 02 Mar")
    if let Some(first_word) = cleaned.split_whitespace().next() {
        if first_word.len() == 3 && !cleaned.starts_with(&format!("{},", first_word)) {
            let with_comma = cleaned.replacen(first_word, &format!("{},", first_word), 1);
            if let Ok(dt) = DateTime::parse_from_rfc2822(&with_comma) {
                return Some(dt.format("%Y-%m-%d %H:%M:%S").to_string());
            }
        }
    }
    
    // Fix: Two-digit year (e.g., "Thu, 11 Jun 09" -> "Thu, 11 Jun 2009")
    let parts: Vec<&str> = cleaned.split_whitespace().collect();
    if parts.len() >= 4 {
        if let Some(year_part) = parts.get(3) {
            if year_part.len() == 2 && year_part.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(year) = year_part.parse::<u32>() {
                    let full_year = if year > 50 { 1900 + year } else { 2000 + year };
                    let fixed = cleaned.replace(&format!(" {} ", year_part), &format!(" {} ", full_year));
                    if let Ok(dt) = DateTime::parse_from_rfc2822(&fixed) {
                        return Some(dt.format("%Y-%m-%d %H:%M:%S").to_string());
                    }
                }
            }
        }
    }
    
    // Fix: ctime format without timezone (e.g., "Thu Jul 20 11:39:51 2006")
    if parts.len() == 5 {
        let format_str = format!("{} {} {} {} {}", parts[0], parts[1], parts[2], parts[3], parts[4]);
        if let Ok(naive) = NaiveDateTime::parse_from_str(&format_str, "%a %b %d %H:%M:%S %Y") {
            return Some(naive.format("%Y-%m-%d %H:%M:%S").to_string());
        }
    }
    
    // Try M/D/YYYY format (e.g., "7/19/2005 8:11:52 AM")
    if cleaned.contains('/') {
        // Try various formats
        let formats = [
            "%m/%d/%Y %I:%M:%S %p",
            "%m/%d/%Y %H:%M:%S",
            "%m/%d/%Y",
        ];
        for fmt in &formats {
            if let Ok(naive) = NaiveDateTime::parse_from_str(&cleaned, fmt) {
                return Some(naive.format("%Y-%m-%d %H:%M:%S").to_string());
            }
        }
    }
    
    None
}

fn should_skip_email(labels: &str, include_spam: bool, include_trash: bool, include_both: bool) -> bool {
    if include_both {
        return false; // Include everything
    }
    
    let labels_lower = labels.to_lowercase();
    let is_spam = labels_lower.contains("spam");
    let is_trash = labels_lower.contains("trash");
    
    if is_spam && !include_spam && !include_both {
        return true;
    }
    
    if is_trash && !include_trash && !include_both {
        return true;
    }
    
    false
}

fn process_mbox(input_path: &PathBuf, output_path: &PathBuf, include_spam: bool, include_trash: bool, include_both: bool) -> Result<()> {
    let file = File::open(input_path)
        .with_context(|| format!("Failed to open input file: {}", input_path.display()))?;
    let reader = BufReader::new(file);

    let mut conn = create_database(output_path)?;

    let tx = conn.transaction()?;

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
            .template("{spinner:.cyan} {msg}")
            .unwrap()
    );
    spinner.set_message("Starting conversion...");

    let mut current_email = Vec::new();
    let mut email_count = 0;
    let mut skipped_count = 0;

    for line in reader.lines() {
        let line = line?;

        if line.starts_with("From ") && !current_email.is_empty() {
            match extract_email_data(&current_email) {
                Ok(record) => {
                    if should_skip_email(&record.gmail_labels, include_spam, include_trash, include_both) {
                        skipped_count += 1;
                    } else {
                        let date_parsed = parse_email_date(&record.date);
                        tx.execute(
                            "INSERT INTO emails (from_addr, to_addr, cc, bcc, subject, date, date_parsed, message_id, in_reply_to, refs, content_type, body_plain, body_html)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                            params![
                                &record.from,
                                &record.to,
                                &record.cc,
                                &record.bcc,
                                &record.subject,
                                &record.date,
                                &date_parsed,
                                &record.message_id,
                                &record.in_reply_to,
                                &record.references,
                                &record.content_type,
                                &record.body_plain,
                                &record.body_html,
                            ],
                        )?;
                        email_count += 1;
                        if email_count % 100 == 0 {
                            spinner.set_message(format!("Processed {} emails ({} skipped)", email_count, skipped_count));
                            spinner.tick();
                        }
                    }
                }
                Err(e) => {
                    spinner.println(format!("Warning: Failed to parse email {}: {}", email_count + skipped_count + 1, e));
                }
            }
            current_email.clear();
        }

        current_email.extend_from_slice(line.as_bytes());
        current_email.push(b'\n');
    }

    if !current_email.is_empty() {
        match extract_email_data(&current_email) {
            Ok(record) => {
                if should_skip_email(&record.gmail_labels, include_spam, include_trash, include_both) {
                    skipped_count += 1;
                } else {
                    let date_parsed = parse_email_date(&record.date);
                    tx.execute(
                        "INSERT INTO emails (from_addr, to_addr, cc, bcc, subject, date, date_parsed, message_id, in_reply_to, refs, content_type, body_plain, body_html)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                        params![
                            &record.from,
                            &record.to,
                            &record.cc,
                            &record.bcc,
                            &record.subject,
                            &record.date,
                            &date_parsed,
                            &record.message_id,
                            &record.in_reply_to,
                            &record.references,
                            &record.content_type,
                            &record.body_plain,
                            &record.body_html,
                        ],
                    )?;
                    email_count += 1;
                }
            }
            Err(e) => {
                spinner.println(format!("Warning: Failed to parse email {}: {}", email_count + skipped_count + 1, e));
            }
        }
    }

    spinner.set_message("Committing to database...");
    spinner.tick();
    tx.commit()?;

    let skip_message = if skipped_count > 0 && !include_both {
        if !include_spam && !include_trash {
            format!("\n    {} Spam/Trash emails skipped (pass --include-spam-and-trash to include them)", skipped_count)
        } else if !include_spam {
            format!("\n    {} Spam emails skipped (pass --include-spam to include them)", skipped_count)
        } else if !include_trash {
            format!("\n    {} Trash emails skipped (pass --include-trash to include them)", skipped_count)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    spinner.finish_with_message(format!("✓ Successfully converted {} emails to database{}", email_count, skip_message));
    println!("Database written to: {}", output_path.display());

    Ok(())
}

fn get_output_path(cli_output: Option<PathBuf>, destructive: bool) -> PathBuf {
    if let Some(path) = cli_output {
        return path;
    }
    
    if destructive {
        return PathBuf::from("emails.db");
    }
    
    let today = Local::now().format("%Y-%m-%d").to_string();
    
    let base_file = PathBuf::from(format!("{}-emails.db", today));
    if !base_file.exists() {
        return base_file;
    }
    
    for counter in 1..10000 {
        let numbered_file = PathBuf::from(format!("{}-emails-{:04}.db", today, counter));
        if !numbered_file.exists() {
            return numbered_file;
        }
    }
    
    base_file
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let output_path = get_output_path(cli.output, cli.destructive);

    process_mbox(
        &cli.input, 
        &output_path, 
        cli.include_spam, 
        cli.include_trash, 
        cli.include_spam_and_trash
    )?;

    Ok(())
}
