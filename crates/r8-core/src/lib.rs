//! Keel/Trellis: the core contract and extension registry.

#![deny(unsafe_op_in_unsafe_fn)]

pub use r8_abi::Value;

pub trait Allocator: Send + Sync {
    fn allocate(&self, size: usize, align: usize) -> *mut u8;
    /// # Safety
    /// `pointer` must come from `allocate` with the same layout.
    unsafe fn deallocate(&self, pointer: *mut u8, size: usize, align: usize);
}

pub struct SystemAllocator;

impl Allocator for SystemAllocator {
    fn allocate(&self, size: usize, align: usize) -> *mut u8 {
        if size == 0 {
            return std::ptr::null_mut();
        }
        // SAFETY: the caller supplies a valid layout and owns the allocation.
        unsafe { std::alloc::alloc(std::alloc::Layout::from_size_align_unchecked(size, align)) }
    }

    unsafe fn deallocate(&self, pointer: *mut u8, size: usize, align: usize) {
        if size == 0 || pointer.is_null() {
            return;
        }
        // SAFETY: the pointer came from `allocate` with the same layout.
        unsafe {
            std::alloc::dealloc(
                pointer,
                std::alloc::Layout::from_size_align_unchecked(size, align),
            )
        }
    }
}

pub trait HostHooks: Send + Sync {
    fn print(&self, value: &str);
}

pub struct NullHost;

impl HostHooks for NullHost {
    fn print(&self, _value: &str) {}
}

pub struct Trellis {
    pub allocator: Box<dyn Allocator>,
    pub host: Box<dyn HostHooks>,
}

impl Trellis {
    pub fn core() -> Self {
        Self {
            allocator: Box::new(SystemAllocator),
            host: Box::new(NullHost),
        }
    }
}

pub struct Isolate {
    trellis: Trellis,
}

impl Isolate {
    pub fn new(trellis: Trellis) -> Self {
        Self { trellis }
    }

    pub fn core() -> Self {
        Self::new(Trellis::core())
    }

    pub fn allocate(&self, size: usize, align: usize) -> *mut u8 {
        self.trellis.allocator.allocate(size, align)
    }

    /// # Safety
    /// `pointer` must come from `allocate` with the same layout.
    pub unsafe fn deallocate(&self, pointer: *mut u8, size: usize, align: usize) {
        // SAFETY: the caller upholds the allocation contract.
        unsafe { self.trellis.allocator.deallocate(pointer, size, align) };
    }

    pub fn print(&self, value: &str) {
        self.trellis.host.print(value);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct CountingAllocator {
        allocations: Arc<AtomicUsize>,
    }

    impl Allocator for CountingAllocator {
        fn allocate(&self, size: usize, align: usize) -> *mut u8 {
            self.allocations.fetch_add(1, Ordering::SeqCst);
            SystemAllocator.allocate(size, align)
        }

        unsafe fn deallocate(&self, pointer: *mut u8, size: usize, align: usize) {
            // SAFETY: the caller upholds the allocation contract.
            unsafe { SystemAllocator.deallocate(pointer, size, align) };
        }
    }

    #[test]
    fn trellis_accepts_a_custom_allocator() {
        let counter = Arc::new(AtomicUsize::new(0));
        let isolate = Isolate::new(Trellis {
            allocator: Box::new(CountingAllocator {
                allocations: Arc::clone(&counter),
            }),
            host: Box::new(NullHost),
        });
        let pointer = isolate.allocate(16, 8);
        assert!(!pointer.is_null());
        // SAFETY: the pointer came from the matching allocation.
        unsafe { isolate.deallocate(pointer, 16, 8) };
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
