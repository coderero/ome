use crate::gateway::{Outbox, SessionId, log};
use ome_protocol::framing::read_frame;
use std::io::{BufReader, BufWriter, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError, sync_channel};
use std::thread;

const OUTBOUND_QUEUE: usize = 16_384;

pub enum Inbound {
    Connected(SessionId, ChannelOutbox),
    Frame(SessionId, Vec<u8>),
    Disconnected(SessionId),
}

pub struct ChannelOutbox(SyncSender<Arc<[u8]>>);

impl Outbox for ChannelOutbox {
    fn send(&self, frame: Arc<[u8]>) -> bool {
        match self.0.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }
}

pub fn listen(listener: TcpListener, inbound: Sender<Inbound>) {
    thread::spawn(move || {
        let mut next_id: SessionId = 0;
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    next_id += 1;
                    if let Err(e) = serve(next_id, stream, inbound.clone()) {
                        log(&format!("session {next_id} setup failed: {e}"));
                    }
                }
                Err(e) => log(&format!("accept failed: {e}")),
            }
        }
    });
}

fn serve(id: SessionId, stream: TcpStream, inbound: Sender<Inbound>) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let writer_stream = stream.try_clone()?;
    let (tx, rx) = sync_channel::<Arc<[u8]>>(OUTBOUND_QUEUE);
    if inbound
        .send(Inbound::Connected(id, ChannelOutbox(tx)))
        .is_err()
    {
        return Ok(());
    }
    log(&format!("session {id} connected from {peer}"));

    thread::spawn(move || write_loop(writer_stream, rx));
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        while let Ok(Some(frame)) = read_frame(&mut reader) {
            if inbound.send(Inbound::Frame(id, frame)).is_err() {
                break;
            }
        }
        let _ = inbound.send(Inbound::Disconnected(id));
    });
    Ok(())
}

fn write_loop(stream: TcpStream, rx: Receiver<Arc<[u8]>>) {
    let mut writer = BufWriter::new(&stream);
    'outer: while let Ok(frame) = rx.recv() {
        if writer.write_all(&frame).is_err() {
            break;
        }
        while let Ok(frame) = rx.try_recv() {
            if writer.write_all(&frame).is_err() {
                break 'outer;
            }
        }
        if writer.flush().is_err() {
            break;
        }
    }
    drop(writer);
    let _ = stream.shutdown(Shutdown::Both);
}
