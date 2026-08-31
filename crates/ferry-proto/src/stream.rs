











use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Condvar, Mutex};






pub trait ByteStream: io::Read + io::Write {}
impl<T: io::Read + io::Write> ByteStream for T {}





pub struct DuplexHalf {
    shared: Arc<DuplexShared>,
    
    
    inbox: usize,
}

struct DuplexShared {
    
    
    state: Mutex<DuplexState>,
    cv: Condvar,
}

struct DuplexState {
    queues: [VecDeque<Vec<u8>>; 2],
    open: bool,
}



pub fn duplex_pair() -> (DuplexHalf, DuplexHalf) {
    let shared = Arc::new(DuplexShared {
        state: Mutex::new(DuplexState {
            queues: [VecDeque::new(), VecDeque::new()],
            open: true,
        }),
        cv: Condvar::new(),
    });
    (
        DuplexHalf {
            shared: Arc::clone(&shared),
            inbox: 1,
        }, 
        DuplexHalf { shared, inbox: 0 }, 
    )
}

impl Drop for DuplexHalf {
    fn drop(&mut self) {
        
        
        
        let mut st = self.shared.state.lock().expect("duplex lock");
        st.open = false;
        self.shared.cv.notify_all();
    }
}

impl DuplexHalf {
    
    
    
    pub fn close(&self) {
        let mut st = self.shared.state.lock().expect("duplex lock");
        st.open = false;
        self.shared.cv.notify_all();
    }

    fn outbox_of(&self) -> usize {
        1 - self.inbox
    }
}

impl io::Read for DuplexHalf {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut st = self.shared.state.lock().expect("duplex lock");
        loop {
            if let Some(record) = st.queues[self.inbox].pop_front() {
                let n = record.len().min(buf.len());
                buf[..n].copy_from_slice(&record[..n]);
                if n < record.len() {
                    
                    
                    st.queues[self.inbox].push_front(record[n..].to_vec());
                }
                return Ok(n);
            }
            if !st.open {
                return Ok(0);
            }
            st = self.shared.cv.wait(st).expect("duplex condvar lock");
        }
    }
}

impl io::Write for DuplexHalf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut st = self.shared.state.lock().expect("duplex lock");
        st.queues[self.outbox_of()].push_back(buf.to_vec());
        drop(st);
        self.shared.cv.notify_all();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn bytes_flow_both_directions_as_records() {
        let (mut a, mut b) = duplex_pair();
        a.write_all(b"ping").unwrap();
        b.write_all(b"pongpong").unwrap();

        let mut buf = [0u8; 4];
        b.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");

        let mut big = [0u8; 8];
        a.read_exact(&mut big).unwrap();
        assert_eq!(&big, b"pongpong");
    }

    #[test]
    fn read_blocks_until_write_then_close_yields_eof() {
        let (mut a, mut b) = duplex_pair();
        let reader = std::thread::spawn(move || {
            let mut got = Vec::new();
            let mut chunk = [0u8; 16];
            loop {
                let n = b.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&chunk[..n]);
            }
            got
        });
        a.write_all(b"streamed").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        a.close();
        assert_eq!(reader.join().unwrap(), b"streamed");
    }

    #[test]
    fn partial_reads_preserve_stream_semantics() {
        let (mut a, mut b) = duplex_pair();
        a.write_all(b"abcdefgh").unwrap();
        
        let mut acc = Vec::new();
        let mut chunk = [0u8; 3];
        while acc.len() < 8 {
            let n = b.read(&mut chunk).unwrap();
            assert!(n > 0);
            acc.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(acc, b"abcdefgh");
    }

    #[test]
    fn close_wakes_a_blocked_reader_on_the_other_half() {
        let (_keep_writer_half_alive, mut b) = duplex_pair();
        
        let reader = std::thread::spawn(move || {
            let mut chunk = [0u8; 4];
            b.read(&mut chunk).map(|_| ())
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(_keep_writer_half_alive);
        let r = reader.join().unwrap();
        assert!(r.is_ok(), "close yields EOF, not an error");
    }
}
