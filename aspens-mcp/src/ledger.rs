//! Append-only, fsynced ledger: the only local state of aspens-mcp.
//!
//! One JSON object per line in `<dir>/ledger.jsonl`. A usage record is written and
//! synced to disk before the order it covers is sent, so a crash can never lose a
//! record whose order went out. Replay rebuilds the in-memory indices on open.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use eyre::{Result, WrapErr, eyre};
use serde::{Deserialize, Serialize};

const LEDGER_FILE: &str = "ledger.jsonl";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

/// Serde adapter writing a `u128` as a decimal string (JSON numbers lose precision).
mod u128_str {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
        let s = String::deserialize(d)?;
        s.parse::<u128>().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fill {
    #[serde(with = "u128_str")]
    pub qty: u128,
    #[serde(with = "u128_str")]
    pub price: u128,
    pub ts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SentOutcome {
    Accepted {
        /// `0x` + 64 hex.
        order_id: String,
        in_book: bool,
        taker_fills: Vec<Fill>,
    },
    Failed {
        code: String,
        message: String,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentRecord {
    pub ts: u64,
    pub market_id: String,
    pub side: Side,
    #[serde(with = "u128_str")]
    pub qty: u128,
    #[serde(with = "u128_str")]
    pub price: u128,
    pub outcome: SentOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    Usage {
        ts: u64,
        /// YYYY-MM-DD, UTC.
        day: String,
        market_id: String,
        side: Side,
        #[serde(with = "u128_str")]
        qty: u128,
        #[serde(with = "u128_str")]
        quote: u128,
    },
    Sent(SentRecord),
    Cancelled {
        ts: u64,
        order_id: String,
    },
    Halt {
        ts: u64,
        reason: Option<String>,
    },
    Resume {
        ts: u64,
    },
}

/// File operations the ledger needs; a seam so tests can inject disk faults.
trait LedgerFile {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;
    fn sync_data(&mut self) -> io::Result<()>;
    fn set_len(&mut self, len: u64) -> io::Result<()>;
}

impl LedgerFile for File {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        Write::write_all(self, buf)
    }
    fn sync_data(&mut self) -> io::Result<()> {
        File::sync_data(self)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        File::set_len(self, len)
    }
}

pub struct Ledger {
    file: Box<dyn LedgerFile>,
    /// Holds the exclusive advisory lock for the ledger's lifetime.
    _lock: Option<File>,
    /// Set when a failed write could not be rolled back; no further appends.
    broken: bool,
    /// Bytes of complete, durable content; a failed write is rolled back to this.
    len: u64,
    usage: HashMap<(String, String), (u128, u128)>,
    sent: Vec<SentRecord>,
    cancelled: HashSet<String>,
    halted: Option<Option<String>>,
}

impl Ledger {
    /// Open (creating `dir` and the file if missing) and replay the ledger.
    pub fn open(dir: &Path) -> Result<Ledger> {
        std::fs::create_dir_all(dir)
            .wrap_err_with(|| format!("creating ledger dir {}", dir.display()))?;
        let path = dir.join(LEDGER_FILE);
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)
            .wrap_err_with(|| format!("opening {}", path.display()))?;
        file.try_lock()
            .map_err(|_| eyre!("another aspens-mcp is using {}", dir.display()))?;
        File::open(dir)
            .and_then(|d| d.sync_all())
            .wrap_err_with(|| format!("syncing {}", dir.display()))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .wrap_err_with(|| format!("reading {}", path.display()))?;

        let complete_len = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let mut ledger = Ledger {
            file: Box::new(file.try_clone()?),
            _lock: None,
            broken: false,
            len: complete_len as u64,
            usage: HashMap::new(),
            sent: Vec::new(),
            cancelled: HashSet::new(),
            halted: None,
        };
        let mut lines: Vec<&[u8]> = bytes[..complete_len].split(|b| *b == b'\n').collect();
        lines.pop(); // empty item after the final newline
        for (i, line) in lines.into_iter().enumerate() {
            let record: Record = serde_json::from_slice(line)
                .map_err(|e| eyre!("{}: line {} is corrupt: {e}", path.display(), i + 1))?;
            ledger.apply(record);
        }
        ledger._lock = Some(file.try_clone()?);
        if complete_len < bytes.len() {
            tracing::warn!(
                path = %path.display(),
                dropped_bytes = bytes.len() - complete_len,
                "ledger ends in a torn line; truncating it"
            );
            file.set_len(complete_len as u64)
                .wrap_err("truncating torn ledger tail")?;
            file.sync_data()?;
        }
        Ok(ledger)
    }

    /// Persist `r` (one write, then fsync) and only then update in-memory state.
    pub fn append(&mut self, r: Record) -> Result<()> {
        let mut line = serde_json::to_vec(&r)?;
        line.push(b'\n');
        if self.broken {
            return Err(eyre!(
                "ledger is broken after a failed rollback; restart required"
            ));
        }
        let written = self
            .file
            .write_all(&line)
            .and_then(|()| self.file.sync_data());
        if let Err(e) = written {
            // Don't leave a partial line that the next append would join onto.
            if self.file.set_len(self.len).is_err() {
                self.broken = true;
            }
            return Err(e).wrap_err("writing ledger record");
        }
        self.len += line.len() as u64;
        self.apply(r);
        Ok(())
    }

    fn apply(&mut self, r: Record) {
        match r {
            Record::Usage {
                day,
                market_id,
                qty,
                quote,
                ..
            } => {
                let e = self.usage.entry((day, market_id)).or_insert((0, 0));
                e.0 = e.0.saturating_add(qty);
                e.1 = e.1.saturating_add(quote);
            }
            Record::Sent(s) => self.sent.push(s),
            Record::Cancelled { order_id, .. } => {
                self.cancelled.insert(order_id);
            }
            Record::Halt { reason, .. } => self.halted = Some(reason),
            Record::Resume { .. } => self.halted = None,
        }
    }

    /// (qty, quote) used on `day` for `market_id`.
    pub fn usage(&self, day: &str, market_id: &str) -> (u128, u128) {
        self.usage
            .get(&(day.to_string(), market_id.to_string()))
            .copied()
            .unwrap_or((0, 0))
    }

    /// `Some(reason)` when the last halt/resume record is a halt.
    pub fn halted(&self) -> Option<Option<String>> {
        self.halted.clone()
    }

    /// Sent records in append order.
    pub fn sent(&self) -> Vec<&SentRecord> {
        self.sent.iter().collect()
    }

    pub fn cancelled(&self, order_id: &str) -> bool {
        self.cancelled.contains(order_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage_rec(day: &str, market: &str, qty: u128, quote: u128) -> Record {
        Record::Usage {
            ts: 1,
            day: day.into(),
            market_id: market.into(),
            side: Side::Buy,
            qty,
            quote,
        }
    }

    fn sent_rec() -> SentRecord {
        SentRecord {
            ts: 7,
            market_id: "m1".into(),
            side: Side::Sell,
            qty: 11,
            price: 13,
            outcome: SentOutcome::Accepted {
                order_id: format!("0x{}", "ab".repeat(32)),
                in_book: true,
                taker_fills: vec![Fill {
                    qty: 3,
                    price: 17,
                    ts: 9,
                }],
            },
        }
    }

    #[test]
    fn append_then_reopen_replays() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Ledger::open(dir.path()).unwrap();
        l.append(usage_rec("2026-11-06", "m1", 5, 50)).unwrap();
        l.append(usage_rec("2026-11-06", "m1", 2, 20)).unwrap();
        l.append(Record::Sent(sent_rec())).unwrap();
        l.append(Record::Cancelled {
            ts: 3,
            order_id: "0xaa".into(),
        })
        .unwrap();
        l.append(Record::Halt {
            ts: 4,
            reason: Some("breaker".into()),
        })
        .unwrap();
        drop(l);
        let l = Ledger::open(dir.path()).unwrap();
        assert_eq!(l.usage("2026-11-06", "m1"), (7, 70));
        assert_eq!(l.usage("2026-11-05", "m1"), (0, 0));
        assert_eq!(l.usage("2026-11-06", "m2"), (0, 0));
        assert_eq!(l.halted(), Some(Some("breaker".to_string())));
        assert_eq!(l.sent(), vec![&sent_rec()]);
        assert!(l.cancelled("0xaa"));
        assert!(!l.cancelled("0xAA"));
    }

    #[test]
    fn torn_tail_is_ignored_and_usage_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Ledger::open(dir.path()).unwrap();
        l.append(usage_rec("2026-11-06", "m1", 5, 50)).unwrap();
        l.append(usage_rec("2026-11-06", "m1", 2, 20)).unwrap();
        drop(l);
        let path = dir.path().join(LEDGER_FILE);
        let third = serde_json::to_string(&usage_rec("2026-11-06", "m1", 100, 1000)).unwrap();
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        Write::write_all(&mut f, &third.as_bytes()[..third.len() / 2]).unwrap();
        drop(f);

        let mut l = Ledger::open(dir.path()).unwrap();
        assert_eq!(l.usage("2026-11-06", "m1"), (7, 70));
        l.append(usage_rec("2026-11-06", "m1", 1, 10)).unwrap();
        drop(l);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.ends_with('\n'));
        let l = Ledger::open(dir.path()).unwrap();
        assert_eq!(l.usage("2026-11-06", "m1"), (8, 80));
    }

    #[test]
    fn corrupt_middle_line_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let good = serde_json::to_string(&usage_rec("2026-11-06", "m1", 5, 50)).unwrap();
        std::fs::write(
            dir.path().join(LEDGER_FILE),
            format!("{good}\nnot json\n{good}\n"),
        )
        .unwrap();
        let err = Ledger::open(dir.path()).err().unwrap().to_string();
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn resume_clears_halt() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Ledger::open(dir.path()).unwrap();
        assert_eq!(l.halted(), None);
        l.append(Record::Halt {
            ts: 1,
            reason: None,
        })
        .unwrap();
        assert_eq!(l.halted(), Some(None));
        l.append(Record::Resume { ts: 2 }).unwrap();
        assert_eq!(l.halted(), None);
        l.append(Record::Halt {
            ts: 3,
            reason: Some("again".into()),
        })
        .unwrap();
        assert_eq!(l.halted(), Some(Some("again".to_string())));
    }

    #[test]
    fn u128_round_trips_above_u64() {
        let big: u128 = 1u128 << 100;
        let dir = tempfile::tempdir().unwrap();
        let mut l = Ledger::open(dir.path()).unwrap();
        l.append(usage_rec("2026-11-06", "m1", big, big + 1))
            .unwrap();
        let mut s = sent_rec();
        s.qty = big + 2;
        l.append(Record::Sent(s.clone())).unwrap();
        drop(l);
        let text = std::fs::read_to_string(dir.path().join(LEDGER_FILE)).unwrap();
        assert!(text.contains(&format!("\"{big}\"")), "{text}");
        let l = Ledger::open(dir.path()).unwrap();
        assert_eq!(l.usage("2026-11-06", "m1"), (big, big + 1));
        assert_eq!(l.sent()[0].qty, big + 2);
    }

    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Faults {
        fail_write: bool,
        fail_set_len: bool,
        written: Vec<u8>,
    }

    struct FaultyFile(Arc<Mutex<Faults>>);

    impl LedgerFile for FaultyFile {
        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            let mut f = self.0.lock().unwrap();
            if f.fail_write {
                return Err(io::Error::other("injected write failure"));
            }
            f.written.extend_from_slice(buf);
            Ok(())
        }
        fn sync_data(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn set_len(&mut self, _len: u64) -> io::Result<()> {
            if self.0.lock().unwrap().fail_set_len {
                return Err(io::Error::other("injected truncate failure"));
            }
            Ok(())
        }
    }

    fn faulty_ledger() -> (Ledger, Arc<Mutex<Faults>>) {
        let faults = Arc::new(Mutex::new(Faults::default()));
        let ledger = Ledger {
            file: Box::new(FaultyFile(faults.clone())),
            _lock: None,
            broken: false,
            len: 0,
            usage: HashMap::new(),
            sent: Vec::new(),
            cancelled: HashSet::new(),
            halted: None,
        };
        (ledger, faults)
    }

    #[test]
    fn failed_append_leaves_state_unchanged() {
        let (mut l, faults) = faulty_ledger();
        l.append(usage_rec("2026-11-06", "m1", 5, 50)).unwrap();
        faults.lock().unwrap().fail_write = true;
        assert!(l.append(usage_rec("2026-11-06", "m1", 2, 20)).is_err());
        assert_eq!(l.usage("2026-11-06", "m1"), (5, 50));
        faults.lock().unwrap().fail_write = false;
        l.append(usage_rec("2026-11-06", "m1", 3, 30)).unwrap();
        assert_eq!(l.usage("2026-11-06", "m1"), (8, 80));
    }

    #[test]
    fn failed_rollback_poisons_ledger() {
        let (mut l, faults) = faulty_ledger();
        {
            let mut f = faults.lock().unwrap();
            f.fail_write = true;
            f.fail_set_len = true;
        }
        let err = l
            .append(usage_rec("2026-11-06", "m1", 2, 20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("writing ledger record"), "{err}");
        {
            let mut f = faults.lock().unwrap();
            f.fail_write = false;
            f.fail_set_len = false;
        }
        let err = l
            .append(usage_rec("2026-11-06", "m1", 3, 30))
            .unwrap_err()
            .to_string();
        assert!(err.contains("restart required"), "{err}");
        assert!(faults.lock().unwrap().written.is_empty());
        assert_eq!(l.usage("2026-11-06", "m1"), (0, 0));
    }

    #[test]
    fn second_open_on_same_dir_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let first = Ledger::open(dir.path()).unwrap();
        let err = Ledger::open(dir.path()).err().unwrap().to_string();
        assert!(err.contains("another aspens-mcp is using"), "{err}");
        drop(first);
        Ledger::open(dir.path()).unwrap();
    }

    #[test]
    fn torn_tail_that_is_complete_json_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let good = serde_json::to_string(&usage_rec("2026-11-06", "m1", 5, 50)).unwrap();
        let unsynced = serde_json::to_string(&usage_rec("2026-11-06", "m1", 9, 90)).unwrap();
        let path = dir.path().join(LEDGER_FILE);
        std::fs::write(&path, format!("{good}\n{unsynced}")).unwrap();
        let l = Ledger::open(dir.path()).unwrap();
        assert_eq!(l.usage("2026-11-06", "m1"), (5, 50));
        drop(l);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{good}\n"));
    }

    #[test]
    fn torn_tail_with_no_complete_line_is_empty_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEDGER_FILE);
        std::fs::write(&path, "{\"kind\":\"usa").unwrap();
        let mut l = Ledger::open(dir.path()).unwrap();
        assert_eq!(l.usage("2026-11-06", "m1"), (0, 0));
        l.append(usage_rec("2026-11-06", "m1", 1, 10)).unwrap();
        drop(l);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
    }

    #[test]
    fn corrupt_last_complete_line_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let good = serde_json::to_string(&usage_rec("2026-11-06", "m1", 5, 50)).unwrap();
        std::fs::write(
            dir.path().join(LEDGER_FILE),
            format!("{good}\n{good}\n{{\"kind\":\"usa\n"),
        )
        .unwrap();
        let err = Ledger::open(dir.path()).err().unwrap().to_string();
        assert!(err.contains("line 3"), "{err}");
    }
}
