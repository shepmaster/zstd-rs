use std::{alloc::Layout, ptr::slice_from_raw_parts};

pub struct RingBuffer {
    buf: *mut u8,
    cap: usize,
    head: usize,
    tail: usize,
}

impl RingBuffer {
    pub fn new() -> Self {
        RingBuffer {
            buf: std::ptr::null_mut(),
            cap: 0,
            head: 0,
            tail: 0,
        }
    }

    pub fn len(&self) -> usize {
        let (x, y) = self.data_slice_lengths();
        x + y
    }

    pub fn free(&self) -> usize {
        let (x, y) = self.free_slice_lengths();
        (x + y).saturating_sub(1)
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    pub fn reserve(&mut self, amount: usize) {
        let free = self.free();
        if free >= amount {
            return;
        }

        self.reserve_amortized(amount - free);
    }

    #[inline(never)]
    #[cold]
    fn reserve_amortized(&mut self, amount: usize) {
        // SAFETY: if we were succesfully able to construct this layout when we allocated then it's also valid do so now
        let current_layout = unsafe { Layout::array::<u8>(self.cap).unwrap_unchecked() };

        // Always have at least 1 unused element as the sentinel.
        let new_cap = usize::max(
            self.cap.next_power_of_two(),
            (self.cap + amount).next_power_of_two(),
        ) + 1;

        // Check that the capacity isn't bigger than isize::MAX, which is the max allowed by LLVM, or that
        // we are on a >= 64 bit system which will never allow that much memory to be allocated
        #[allow(clippy::assertions_on_constants)]
        {
            debug_assert!(usize::BITS >= 64 || new_cap < isize::MAX as usize);
        }

        let new_layout = Layout::array::<u8>(new_cap)
            .unwrap_or_else(|_| panic!("Could not create layout for u8 array of size {}", new_cap));

        // alloc the new memory region and panic if alloc fails
        // TODO maybe rework this to generate an error?
        let new_buf = unsafe {
            let new_buf = std::alloc::alloc(new_layout);

            if new_buf.is_null() {
                panic!("Allocating new space for the ringbuffer failed");
            }
            new_buf
        };

        // If we had data before, copy it over to the newly alloced memory region
        if self.cap > 0 {
            let ((s1_ptr, s1_len), (s2_ptr, s2_len)) = self.data_slice_parts();

            unsafe {
                new_buf.copy_from_nonoverlapping(s1_ptr, s1_len);
                new_buf.add(s1_len).copy_from_nonoverlapping(s2_ptr, s2_len);
                std::alloc::dealloc(self.buf, current_layout);
            }

            self.tail = s1_len + s2_len;
            self.head = 0;
        }
        self.buf = new_buf;
        self.cap = new_cap;
    }

    #[allow(dead_code)]
    pub fn push_back(&mut self, byte: u8) {
        self.reserve(1);

        unsafe { self.buf.add(self.tail).write(byte) };
        self.tail = (self.tail + 1) % self.cap;
    }

    #[allow(dead_code)]
    pub fn get(&self, idx: usize) -> Option<u8> {
        if idx < self.len() {
            let idx = (self.head + idx) % self.cap;
            Some(unsafe { self.buf.add(idx).read() })
        } else {
            None
        }
    }

    pub fn extend(&mut self, data: &[u8]) {
        let len = data.len();
        let ptr = data.as_ptr();

        self.reserve(len);

        let ((f1_ptr, f1_len), (f2_ptr, f2_len)) = self.free_slice_parts();
        debug_assert!(f1_len + f2_len >= len, "{} + {} < {}", f1_len, f2_len, len);

        let in_f1 = usize::min(len, f1_len);
        let in_f2 = len - in_f1;

        debug_assert!(in_f1 + in_f2 == len);

        unsafe {
            if in_f1 > 0 {
                f1_ptr.copy_from_nonoverlapping(ptr, in_f1);
            }
            if in_f2 > 0 {
                f2_ptr.copy_from_nonoverlapping(ptr.add(in_f1), in_f2);
            }
        }
        self.tail = (self.tail + len) % self.cap;
    }

    pub fn drop_first_n(&mut self, amount: usize) {
        debug_assert!(amount <= self.len());
        let amount = usize::min(amount, self.len());
        self.head = (self.head + amount) % self.cap;
    }

    fn data_slice_lengths(&self) -> (usize, usize) {
        let len_after_head;
        let len_to_tail;

        // TODO can we do this branchless?
        if self.tail >= self.head {
            len_after_head = self.tail - self.head;
            len_to_tail = 0;
        } else {
            len_after_head = self.cap - self.head;
            len_to_tail = self.tail;
        }
        (len_after_head, len_to_tail)
    }

    fn data_slice_parts(&self) -> ((*const u8, usize), (*const u8, usize)) {
        let (len_after_head, len_to_tail) = self.data_slice_lengths();

        (
            (unsafe { self.buf.add(self.head) }, len_after_head),
            (self.buf, len_to_tail),
        )
    }
    pub fn as_slices(&self) -> (&[u8], &[u8]) {
        let (s1, s2) = self.data_slice_parts();
        unsafe {
            let s1 = &*slice_from_raw_parts(s1.0, s1.1);
            let s2 = &*slice_from_raw_parts(s2.0, s2.1);
            (s1, s2)
        }
    }

    fn free_slice_lengths(&self) -> (usize, usize) {
        let len_to_head;
        let len_after_tail;

        // TODO can we do this branchless?
        if self.tail < self.head {
            len_after_tail = self.head - self.tail;
            len_to_head = 0;
        } else {
            len_after_tail = self.cap - self.tail;
            len_to_head = self.head;
        }
        (len_to_head, len_after_tail)
    }

    fn free_slice_parts(&self) -> ((*mut u8, usize), (*mut u8, usize)) {
        let (len_to_head, len_after_tail) = self.free_slice_lengths();

        (
            (unsafe { self.buf.add(self.tail) }, len_after_tail),
            (self.buf, len_to_head),
        )
    }

    #[allow(dead_code)]
    pub fn extend_from_within(&mut self, start: usize, len: usize) {
        if start + len > self.len() {
            panic!(
                "Calls to this functions must respect start ({}) + len ({}) <= self.len() ({})!",
                start,
                len,
                self.len()
            );
        }

        self.reserve(len);
        unsafe { self.extend_from_within_unchecked(start, len) }
    }

    /// SAFETY:
    /// Needs start + len <= self.len()
    /// And more then len reserved space
    #[warn(unsafe_op_in_unsafe_fn)]
    pub unsafe fn extend_from_within_unchecked(&mut self, start: usize, len: usize) {
        debug_assert!(!self.buf.is_null());

        if self.head < self.tail {
            // continous data slice  |____HDDDDDDDT_____|
            let after_tail = usize::min(len, self.cap - self.tail);
            unsafe {
                self.buf
                    .add(self.tail)
                    .copy_from_nonoverlapping(self.buf.add(self.head + start), after_tail);
                if after_tail < len {
                    self.buf.copy_from_nonoverlapping(
                        self.buf.add(self.head + start + after_tail),
                        len - after_tail,
                    );
                }
            }
        } else {
            // continous free slice |DDDT_________HDDDD|
            if self.head + start > self.cap {
                let start = (self.head + start) % self.cap;
                unsafe {
                    self.buf
                        .add(self.tail)
                        .copy_from_nonoverlapping(self.buf.add(start), len)
                }
            } else {
                let after_head = usize::min(len, self.cap - self.head);
                unsafe {
                    self.buf
                        .add(self.tail)
                        .copy_from_nonoverlapping(self.buf.add(self.head + start), after_head);
                    if after_head < len {
                        self.buf
                            .add(self.tail + after_head)
                            .copy_from_nonoverlapping(self.buf, len - after_head);
                    }
                }
            }
        }

        self.tail = (self.tail + len) % self.cap;
    }

    #[allow(dead_code)]
    /// SAFETY:
    /// Needs start + len <= self.len()
    /// And more then len reserved space
    pub unsafe fn extend_from_within_unchecked_branchless(&mut self, start: usize, len: usize) {
        // data slices in raw parts
        let ((s1_ptr, s1_len), (s2_ptr, s2_len)) = self.data_slice_parts();

        debug_assert!(len <= s1_len + s2_len, "{} > {} + {}", len, s1_len, s2_len);

        // calc the actually wanted slices in raw parts
        let start_in_s1 = usize::min(s1_len, start);
        let end_in_s1 = usize::min(s1_len, start + len);
        let m1_ptr = s1_ptr.add(start_in_s1);
        let m1_len = end_in_s1 - start_in_s1;

        debug_assert!(end_in_s1 <= s1_len);
        debug_assert!(start_in_s1 <= s1_len);

        let start_in_s2 = start.saturating_sub(s1_len);
        let end_in_s2 = start_in_s2 + (len - m1_len);
        let m2_ptr = s2_ptr.add(start_in_s2);
        let m2_len = end_in_s2 - start_in_s2;

        debug_assert!(start_in_s2 <= s2_len);
        debug_assert!(end_in_s2 <= s2_len);

        debug_assert_eq!(len, m1_len + m2_len);

        // the free slices, must hold: f1_len + f2_len >= m1_len + m2_len
        let ((f1_ptr, f1_len), (f2_ptr, f2_len)) = self.free_slice_parts();

        debug_assert!(f1_len + f2_len >= m1_len + m2_len);

        // calc how many from where bytes go where
        let m1_in_f1 = usize::min(m1_len, f1_len);
        let m1_in_f2 = m1_len - m1_in_f1;
        let m2_in_f1 = usize::min(f1_len - m1_in_f1, m2_len);
        let m2_in_f2 = m2_len - m2_in_f1;

        debug_assert_eq!(m1_len, m1_in_f1 + m1_in_f2);
        debug_assert_eq!(m2_len, m2_in_f1 + m2_in_f2);
        debug_assert!(f1_len >= m1_in_f1 + m2_in_f1);
        debug_assert!(f2_len >= m1_in_f2 + m2_in_f2);
        debug_assert_eq!(len, m1_in_f1 + m2_in_f1 + m1_in_f2 + m2_in_f2);

        debug_assert!((m1_in_f2 > 0) ^ (m2_in_f1 > 0) || (m1_in_f2 == 0 && m2_in_f1 == 0));

        copy_with_checks(
            m1_ptr, m2_ptr, f1_ptr, f2_ptr, m1_in_f1, m2_in_f1, m1_in_f2, m2_in_f2,
        );
        self.tail = (self.tail + len) % self.cap;
    }
}

impl Drop for RingBuffer {
    fn drop(&mut self) {
        if self.cap == 0 {
            return;
        }

        // SAFETY: is we were succesfully able to construct this layout when we allocated then it's also valid do so now
        let current_layout = unsafe { Layout::array::<u8>(self.cap).unwrap_unchecked() };

        unsafe {
            std::alloc::dealloc(self.buf, current_layout);
        }
    }
}

#[allow(dead_code)]
#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn copy_without_checks(
    m1_ptr: *const u8,
    m2_ptr: *const u8,
    f1_ptr: *mut u8,
    f2_ptr: *mut u8,
    m1_in_f1: usize,
    m2_in_f1: usize,
    m1_in_f2: usize,
    m2_in_f2: usize,
) {
    f1_ptr.copy_from_nonoverlapping(m1_ptr, m1_in_f1);
    f1_ptr
        .add(m1_in_f1)
        .copy_from_nonoverlapping(m2_ptr, m2_in_f1);

    f2_ptr.copy_from_nonoverlapping(m1_ptr.add(m1_in_f1), m1_in_f2);
    f2_ptr
        .add(m1_in_f2)
        .copy_from_nonoverlapping(m2_ptr.add(m2_in_f1), m2_in_f2);
}

#[allow(dead_code)]
#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn copy_with_checks(
    m1_ptr: *const u8,
    m2_ptr: *const u8,
    f1_ptr: *mut u8,
    f2_ptr: *mut u8,
    m1_in_f1: usize,
    m2_in_f1: usize,
    m1_in_f2: usize,
    m2_in_f2: usize,
) {
    if m1_in_f1 != 0 {
        f1_ptr.copy_from_nonoverlapping(m1_ptr, m1_in_f1);
    }
    if m2_in_f1 != 0 {
        f1_ptr
            .add(m1_in_f1)
            .copy_from_nonoverlapping(m2_ptr, m2_in_f1);
    }

    if m1_in_f2 != 0 {
        f2_ptr.copy_from_nonoverlapping(m1_ptr.add(m1_in_f1), m1_in_f2);
    }
    if m2_in_f2 != 0 {
        f2_ptr
            .add(m1_in_f2)
            .copy_from_nonoverlapping(m2_ptr.add(m2_in_f1), m2_in_f2);
    }
}

#[allow(dead_code)]
#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn copy_with_nobranch_check(
    m1_ptr: *const u8,
    m2_ptr: *const u8,
    f1_ptr: *mut u8,
    f2_ptr: *mut u8,
    m1_in_f1: usize,
    m2_in_f1: usize,
    m1_in_f2: usize,
    m2_in_f2: usize,
) {
    let case = (m1_in_f1 > 0) as usize
        | (((m2_in_f1 > 0) as usize) << 1)
        | (((m1_in_f2 > 0) as usize) << 2)
        | (((m2_in_f2 > 0) as usize) << 3);

    match case {
        0 => {}

        // one bit set
        1 => {
            f1_ptr.copy_from_nonoverlapping(m1_ptr, m1_in_f1);
        }
        2 => {
            f1_ptr.copy_from_nonoverlapping(m2_ptr, m2_in_f1);
        }
        4 => {
            f2_ptr.copy_from_nonoverlapping(m1_ptr, m1_in_f2);
        }
        8 => {
            f2_ptr.copy_from_nonoverlapping(m2_ptr, m2_in_f2);
        }

        // two bit set
        3 => {
            f1_ptr.copy_from_nonoverlapping(m1_ptr, m1_in_f1);
            f1_ptr
                .add(m1_in_f1)
                .copy_from_nonoverlapping(m2_ptr, m2_in_f1);
        }
        5 => {
            f1_ptr.copy_from_nonoverlapping(m1_ptr, m1_in_f1);
            f2_ptr.copy_from_nonoverlapping(m1_ptr.add(m1_in_f1), m1_in_f2);
        }
        6 => std::hint::unreachable_unchecked(),
        7 => std::hint::unreachable_unchecked(),
        9 => {
            f1_ptr.copy_from_nonoverlapping(m1_ptr, m1_in_f1);
            f2_ptr.copy_from_nonoverlapping(m2_ptr, m2_in_f2);
        }
        10 => {
            f1_ptr.copy_from_nonoverlapping(m2_ptr, m2_in_f1);
            f2_ptr.copy_from_nonoverlapping(m2_ptr.add(m2_in_f1), m2_in_f2);
        }
        12 => {
            f2_ptr.copy_from_nonoverlapping(m1_ptr, m1_in_f2);
            f2_ptr
                .add(m1_in_f2)
                .copy_from_nonoverlapping(m2_ptr, m2_in_f2);
        }

        // three bit set
        11 => {
            f1_ptr.copy_from_nonoverlapping(m1_ptr, m1_in_f1);
            f1_ptr
                .add(m1_in_f1)
                .copy_from_nonoverlapping(m2_ptr, m2_in_f1);
            f2_ptr.copy_from_nonoverlapping(m2_ptr.add(m2_in_f1), m2_in_f2);
        }
        13 => {
            f1_ptr.copy_from_nonoverlapping(m1_ptr, m1_in_f1);
            f2_ptr.copy_from_nonoverlapping(m1_ptr.add(m1_in_f1), m1_in_f2);
            f2_ptr
                .add(m1_in_f2)
                .copy_from_nonoverlapping(m2_ptr, m2_in_f2);
        }
        14 => std::hint::unreachable_unchecked(),
        15 => std::hint::unreachable_unchecked(),
        _ => std::hint::unreachable_unchecked(),
    }
}

#[cfg(test)]
mod tests {
    use super::RingBuffer;

    #[test]
    fn smoke() {
        let mut rb = RingBuffer::new();

        rb.reserve(15);
        assert_eq!(17, rb.cap);

        rb.extend(b"0123456789");
        assert_eq!(rb.len(), 10);
        assert_eq!(rb.as_slices().0, b"0123456789");
        assert_eq!(rb.as_slices().1, b"");

        rb.drop_first_n(5);
        assert_eq!(rb.len(), 5);
        assert_eq!(rb.as_slices().0, b"56789");
        assert_eq!(rb.as_slices().1, b"");

        rb.extend_from_within(2, 3);
        assert_eq!(rb.len(), 8);
        assert_eq!(rb.as_slices().0, b"56789789");
        assert_eq!(rb.as_slices().1, b"");

        rb.extend_from_within(0, 3);
        assert_eq!(rb.len(), 11);
        assert_eq!(rb.as_slices().0, b"56789789567");
        assert_eq!(rb.as_slices().1, b"");

        rb.extend_from_within(0, 2);
        assert_eq!(rb.len(), 13);
        assert_eq!(rb.as_slices().0, b"567897895675");
        assert_eq!(rb.as_slices().1, b"6");

        rb.drop_first_n(11);
        assert_eq!(rb.len(), 2);
        assert_eq!(rb.as_slices().0, b"5");
        assert_eq!(rb.as_slices().1, b"6");

        rb.extend(b"0123456789");
        assert_eq!(rb.len(), 12);
        assert_eq!(rb.as_slices().0, b"5");
        assert_eq!(rb.as_slices().1, b"60123456789");

        rb.drop_first_n(11);
        assert_eq!(rb.len(), 1);
        assert_eq!(rb.as_slices().0, b"9");
        assert_eq!(rb.as_slices().1, b"");

        rb.extend(b"0123456789");
        assert_eq!(rb.len(), 11);
        assert_eq!(rb.as_slices().0, b"9012345");
        assert_eq!(rb.as_slices().1, b"6789");
    }

    #[test]
    fn edge_cases() {
        // Fill exactly, then empty then fill again
        let mut rb = RingBuffer::new();
        rb.reserve(16);
        assert_eq!(17, rb.cap);
        rb.extend(b"0123456789012345");
        assert_eq!(17, rb.cap);
        assert_eq!(16, rb.len());
        assert_eq!(0, rb.free());
        rb.drop_first_n(16);
        assert_eq!(0, rb.len());
        assert_eq!(16, rb.free());
        rb.extend(b"0123456789012345");
        assert_eq!(16, rb.len());
        assert_eq!(0, rb.free());
        assert_eq!(17, rb.cap);
        assert_eq!(1, rb.as_slices().0.len());
        assert_eq!(15, rb.as_slices().1.len());

        rb.clear();

        // data in both slices and then reserve
        rb.extend(b"0123456789012345");
        rb.drop_first_n(8);
        rb.extend(b"67890123");
        assert_eq!(16, rb.len());
        assert_eq!(0, rb.free());
        assert_eq!(17, rb.cap);
        assert_eq!(9, rb.as_slices().0.len());
        assert_eq!(7, rb.as_slices().1.len());
        rb.reserve(1);
        assert_eq!(16, rb.len());
        assert_eq!(16, rb.free());
        assert_eq!(33, rb.cap);
        assert_eq!(16, rb.as_slices().0.len());
        assert_eq!(0, rb.as_slices().1.len());

        rb.clear();

        // fill exactly, then extend from within
        rb.extend(b"0123456789012345");
        rb.extend_from_within(0, 16);
        assert_eq!(32, rb.len());
        assert_eq!(0, rb.free());
        assert_eq!(33, rb.cap);
        assert_eq!(32, rb.as_slices().0.len());
        assert_eq!(0, rb.as_slices().1.len());

        // extend from within cases
        let mut rb = RingBuffer::new();
        rb.reserve(8);
        rb.extend(b"01234567");
        rb.drop_first_n(5);
        rb.extend_from_within(0, 3);
        assert_eq!(4, rb.as_slices().0.len());
        assert_eq!(2, rb.as_slices().1.len());

        rb.drop_first_n(2);
        assert_eq!(2, rb.as_slices().0.len());
        assert_eq!(2, rb.as_slices().1.len());
        rb.extend_from_within(0, 4);
        assert_eq!(2, rb.as_slices().0.len());
        assert_eq!(6, rb.as_slices().1.len());

        rb.drop_first_n(2);
        assert_eq!(6, rb.as_slices().0.len());
        assert_eq!(0, rb.as_slices().1.len());
        rb.drop_first_n(2);
        assert_eq!(4, rb.as_slices().0.len());
        assert_eq!(0, rb.as_slices().1.len());
        rb.extend_from_within(0, 4);
        assert_eq!(7, rb.as_slices().0.len());
        assert_eq!(1, rb.as_slices().1.len());
    }
}