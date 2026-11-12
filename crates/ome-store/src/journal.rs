use ome_protocol::DecodeError;
use ome_protocol::framing::{SOFH_LEN, frame, read_frame};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::Path;

pub trait Entry: Sized {
    fn encode(&self) -> Vec<u8>;
    fn decode(bytes: &[u8]) -> Result<Self, DecodeError>;
}

pub struct Journal<E> {
    writer: BufWriter<File>,
    _entries: PhantomData<E>,
}

impl<E: Entry> Journal<E> {
    pub fn open(path: &Path) -> io::Result<(Self, Vec<E>)> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;

        let mut entries = Vec::new();
        let mut good_len = 0u64;
        {
            let mut reader = BufReader::new(&mut file);
            loop {
                match read_frame(&mut reader) {
                    Ok(Some(payload)) => {
                        let entry = E::decode(&payload).map_err(|e| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("corrupt journal entry #{}: {e}", entries.len() + 1),
                            )
                        })?;
                        entries.push(entry);
                        good_len += (payload.len() + SOFH_LEN) as u64;
                    }
                    Ok(None) => break,
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(e),
                }
            }
        }

        if file.metadata()?.len() > good_len {
            file.set_len(good_len)?;
        }
        file.seek(SeekFrom::End(0))?;
        Ok((
            Self {
                writer: BufWriter::new(file),
                _entries: PhantomData,
            },
            entries,
        ))
    }

    pub fn append(&mut self, entry: &E) -> io::Result<()> {
        self.writer.write_all(&frame(&entry.encode()))
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ome_protocol::{Decoder, Encoder};

    #[derive(Debug, PartialEq)]
    struct Note(u64, String);

    impl Entry for Note {
        fn encode(&self) -> Vec<u8> {
            Encoder::new(900)
                .u64(self.0)
                .end_block()
                .var(&self.1)
                .finish()
        }

        fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
            let mut d = Decoder::new(bytes)?;
            let n = d.u64()?;
            d.end_block()?;
            Ok(Note(n, d.var()?))
        }
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ome-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("test.journal")
    }

    #[test]
    fn appended_entries_are_read_back_in_order() {
        let path = temp("order");
        {
            let (mut journal, entries) = Journal::<Note>::open(&path).unwrap();
            assert!(entries.is_empty());
            journal.append(&Note(1, "a".into())).unwrap();
            journal.append(&Note(2, "b".into())).unwrap();
            journal.flush().unwrap();
        }
        let (_, entries) = Journal::<Note>::open(&path).unwrap();
        assert_eq!(entries, vec![Note(1, "a".into()), Note(2, "b".into())]);
    }

    #[test]
    fn torn_tail_is_truncated_on_open() {
        let path = temp("torn");
        {
            let (mut journal, _) = Journal::<Note>::open(&path).unwrap();
            journal.append(&Note(1, "ok".into())).unwrap();
            journal.flush().unwrap();
        }
        let good_len = std::fs::metadata(&path).unwrap().len();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[0, 0, 0, 40, 0x5B])
            .unwrap();

        let (mut journal, entries) = Journal::<Note>::open(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), good_len);
        journal.append(&Note(2, "after".into())).unwrap();
        journal.flush().unwrap();
        assert_eq!(Journal::<Note>::open(&path).unwrap().1.len(), 2);
    }

    #[test]
    fn corrupt_entry_is_an_error() {
        let path = temp("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, frame(b"garbage")).unwrap();
        assert!(Journal::<Note>::open(&path).is_err());
    }
}
