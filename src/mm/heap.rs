use buddy_system_allocator::Heap;
use core::ops::RangeInclusive;
use spin::Mutex;

struct LockedHeap<const ORDER: usize>(Mutex<Heap<ORDER>>);

impl<const ORDER: usize> LockedHeap<ORDER> {
    /// Creates an empty heap
    pub const fn empty() -> Self {
        LockedHeap(Mutex::new(Heap::<ORDER>::new()))
    }
}

impl<const ORDER: usize> core::ops::Deref for LockedHeap<ORDER> {
    type Target = Mutex<Heap<ORDER>>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

unsafe impl<const ORDER: usize> alloc::alloc::GlobalAlloc for LockedHeap<ORDER> {
    unsafe fn alloc(&self, layout: alloc::alloc::Layout) -> *mut u8 {
        self.0
            .lock()
            .alloc(layout)
            .map_or(core::ptr::null_mut(), |allocation| allocation.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: alloc::alloc::Layout) {
        self.0.lock().dealloc(core::ptr::NonNull::new_unchecked(ptr), layout)
    }
}

#[global_allocator]
static HEAP_ALLOCATOR: LockedHeap<{ usize::BITS as usize }> = LockedHeap::empty();

pub fn heap_init() {
    #[repr(align(4096))]
    struct HeapRegion([u8; HEAP_SIZE]);
    const HEAP_SIZE: usize = (1 << 20) * 4;
    static mut HEAP_REGION: HeapRegion = HeapRegion([0; HEAP_SIZE]);

    unsafe {
        info!("init buddy system: {:?}", HEAP_REGION.0.as_ptr_range());
        HEAP_ALLOCATOR.lock().init(HEAP_REGION.0.as_ptr() as usize, HEAP_SIZE);
    }
}

// make sure that the va ranges has corresponding physical pages
pub fn heap_expansion(region: RangeInclusive<usize>) {
    info!("heap_expansion: {:#x}..={:#x}", region.start(), region.end());
    unsafe {
        HEAP_ALLOCATOR.lock().add_to_heap(*region.start(), *region.end() + 1);
    }
}
