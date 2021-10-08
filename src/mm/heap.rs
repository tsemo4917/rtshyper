// rCore buddy system allocator
use crate::arch::PAGE_SIZE;
use buddy_system_allocator::LockedHeap;

const HEAP_SIZE: usize = 1024 * PAGE_SIZE;

#[repr(align(4096))]
struct HeapRegion([u8; HEAP_SIZE]);

static HEAP_REGION: HeapRegion = HeapRegion([0; HEAP_SIZE]);

#[global_allocator]
pub static HEAP_ALLOCATOR: LockedHeap<32> = LockedHeap::empty();

pub fn heap_init() {
    println!("init buddy system");
    unsafe {
        HEAP_ALLOCATOR
            .lock()
            .init(&HEAP_REGION.0 as *const _ as usize, HEAP_SIZE);
    }
}

#[alloc_error_handler]
fn alloc_error_handler(_: core::alloc::Layout) -> ! {
    panic!("alloc_error_handler: heap panic");
}
