/// A simple ring buffer for recording incoming audio from the DAW.
pub struct RingBuffer {
    buffer: Vec<f32>,
    write_pos: usize,
    len: usize,
}

impl RingBuffer {
    /// Create a new ring buffer with the given capacity in samples.
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: vec![0.0; capacity],
            write_pos: 0,
            len: 0,
        }
    }

    /// Write samples into the ring buffer.
    pub fn write(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.buffer[self.write_pos] = sample;
            self.write_pos = (self.write_pos + 1) % self.buffer.len();
            if self.len < self.buffer.len() {
                self.len += 1;
            }
        }
    }

    /// Read all recorded samples in chronological order.
    pub fn read(&self) -> Vec<f32> {
        if self.len < self.buffer.len() {
            // Buffer hasn't wrapped yet
            self.buffer[..self.len].to_vec()
        } else {
            // Buffer has wrapped: read from write_pos to end, then start to write_pos
            let mut out = Vec::with_capacity(self.buffer.len());
            out.extend_from_slice(&self.buffer[self.write_pos..]);
            out.extend_from_slice(&self.buffer[..self.write_pos]);
            out
        }
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        self.write_pos = 0;
        self.len = 0;
    }

    /// Number of samples currently stored.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Total capacity in samples.
    pub fn capacity(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_and_read() {
        let mut buf = RingBuffer::new(8);
        buf.write(&[1.0, 2.0, 3.0]);
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.read(), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_wrap_around() {
        let mut buf = RingBuffer::new(4);
        buf.write(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.read(), vec![3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn test_clear() {
        let mut buf = RingBuffer::new(4);
        buf.write(&[1.0, 2.0]);
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.read(), Vec::<f32>::new());
    }
}
