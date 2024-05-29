use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn main() {
    let b = Arc::new(OneshotRingBuffer::new(8));
    let b2 = Arc::clone(&b);
    let t1 = std::thread::spawn(move || {
        let buf = unsafe { b.prepare_send(4) };
        buf.copy_from_slice(&[1, 2, 3, 4]);
        unsafe { b.send(4) };

        let buf = unsafe { b.prepare_send(4) };
        buf.copy_from_slice(&[5, 6, 7, 8]);
        unsafe { b.send(4) };
    });

    let mut all_bytes = vec![];
    while let Some(buf) = unsafe { b2.recv() } {
        all_bytes.extend_from_slice(buf);
    }

    t1.join().unwrap();
    assert_eq!(all_bytes, &[1, 2, 3, 4, 5, 6, 7, 8]);
}

struct OneshotRingBuffer {
    buffer: Box<[UnsafeCell<u8>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

unsafe impl Send for OneshotRingBuffer {}
unsafe impl Sync for OneshotRingBuffer {}

impl OneshotRingBuffer {
    fn new(capacity: usize) -> Self {
        let buffer: Vec<UnsafeCell<u8>> = (0..capacity).map(|_| UnsafeCell::new(0u8)).collect();

        Self {
            buffer: buffer.into_boxed_slice(),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Returns a mutable slice of the next `count` bytes from the ring buffer
    /// Call `Self::send` to make bytes visible to the reader
    ///
    /// SAFETY:
    /// Cannot be called concurrently from multiple threads
    unsafe fn prepare_send(&self, len: usize) -> &mut [u8] {
        assert!(len != 0);

        let idx = self.head.load(Ordering::Acquire);
        if idx + len > self.buffer.len() {
            panic!("Buffer out of space!");
        }
        let start = self.buffer[idx].get();
        // TODO: Communicate to miri that we are accessing self.buffer[idx..idx+start]

        // `cargo miri r` currently fails with:
        // error: Undefined Behavior: trying to retag from <8042> for Unique permission at alloc1214[0x1], but that tag does not exist in the borrow stack for this location
        //     --> /home/troy/.rustup/toolchains/nightly-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/slice/raw.rs:162:9
        //      |
        // 162  |         &mut *ptr::slice_from_raw_parts_mut(data, len)
        //      |         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
        //      |         |
        //      |         trying to retag from <8042> for Unique permission at alloc1214[0x1], but that tag does not exist in the borrow stack for this location
        //      |         this error occurs as part of retag at alloc1214[0x0..0x4]
        //      |
        //      = help: this indicates a potential bug in the program: it performed an invalid operation, but the Stacked Borrows rules it violated are still experimental
        //      = help: see https://github.com/rust-lang/unsafe-code-guidelines/blob/master/wip/stacked-borrows.md for further information
        // help: <8042> was created by a SharedReadWrite retag at offsets [0x0..0x1]
        //     --> /home/troy/.rustup/toolchains/nightly-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/cell.rs:2127:9
        //      |
        // 2127 |         self as *const UnsafeCell<T> as *const T as *mut T


        // SAFETY:
        // 1. We have not advanced head yet, therefore the reader cannot read at or after `start`
        // 2. By our contract, this function is not executing concurrently
        // Therefore we have exclusive access to `start..start+count`
        unsafe { std::slice::from_raw_parts_mut(start, len) }
    }

    /// SAFETY:
    /// 1. Must be preceded by a call to `Self::prepare_send` with the same value for count
    /// 2. Cannot be called concurrently from multiple threads
    unsafe fn send(&self, count: usize) {
        self.head.fetch_add(count, Ordering::AcqRel);
    }

    /// Spins until some bytes are available from the sender.
    /// Returns `None` if all bytes have been received.
    ///
    /// SAFETY:
    /// Cannot be called concurrently
    unsafe fn recv(&self) -> Option<&[u8]> {
        loop {
            let tail = self.tail.load(Ordering::Acquire);
            let head = self.head.load(Ordering::Acquire);
            if tail == self.buffer.len() {
                return None;
            }
            if head != tail {
                let len = head - tail;
                let start = self.buffer[tail].get();
                self.tail.fetch_add(len, Ordering::AcqRel);

                // XXX: Same issue as above for creating the slice
                break Some(unsafe { std::slice::from_raw_parts(start, len) });
            }
            std::hint::spin_loop();
        }
    }
}
