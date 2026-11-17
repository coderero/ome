pub mod journal;

pub use journal::{Entry, Journal};

pub trait FactLog<E>: Send {
    fn append(&mut self, entry: &E) -> std::io::Result<()>;
    fn flush(&mut self) -> std::io::Result<()>;
}

impl<E: Entry + Send> FactLog<E> for Journal<E> {
    fn append(&mut self, entry: &E) -> std::io::Result<()> {
        Journal::append(self, entry)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Journal::flush(self)
    }
}

pub struct MemoryLog<E> {
    pub entries: std::sync::Arc<std::sync::Mutex<Vec<E>>>,
}

impl<E: Clone + Send> FactLog<E> for MemoryLog<E> {
    fn append(&mut self, entry: &E) -> std::io::Result<()> {
        self.entries.lock().expect("log lock").push(entry.clone());
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn boxed<E: Entry + Send + 'static>(journal: Journal<E>) -> Box<dyn FactLog<E>> {
    Box::new(journal)
}
