use core::mem::size_of;
use core::ops::RangeInclusive;

use alloc::vec::Vec;
use spin::{Mutex, Once};

use crate::arch::{
    cache_init, Arch, ArchTrait, CacheInfoTrait, CacheInvalidate, TlbInvalidate, CPU_CACHE, PAGE_SHIFT, PAGE_SIZE,
    PTE_S1_DEVICE, PTE_S1_NORMAL,
};
use crate::board::*;
use crate::kernel::Cpu;
use crate::mm::vpage_allocator::{vpage_alloc, AllocatedPages, CPU_BANKED_ADDRESS};
use crate::mm::{_image_end, _image_start, heap_expansion, PageFrame};
use crate::util::{barrier, reset_barrier, round_up};

use super::{current_cpu, CPU_MASTER};

pub static HYPERVISOR_COLORS: Once<Vec<usize>> = Once::new();

pub fn physical_mem_init() {
    cache_init();
    mem_region_init_by_colors();
    info!("Mem init ok");
}

#[derive(Debug)]
pub enum AllocError {
    AllocZeroPage,
    OutOfFrame(usize),
}

pub fn mem_page_alloc() -> Result<PageFrame, AllocError> {
    PageFrame::alloc_pages(1)
}

#[allow(dead_code)]
pub fn mem_pages_alloc(page_num: usize) -> Result<PageFrame, AllocError> {
    PageFrame::alloc_pages(page_num)
}

#[derive(Clone, Debug)]
pub struct ColorMemRegion {
    pub color: usize,
    pub base: usize,
    pub count: usize,
    pub step: usize,
    available: bool,
}

impl ColorMemRegion {
    fn new(color: usize, base: usize, count: usize, step: usize) -> Self {
        Self {
            color,
            base,
            count,
            step,
            available: true,
        }
    }

    fn left_neighbor(&self, other: &Self) -> bool {
        self.base + self.count * self.step == other.base
    }

    fn is_available(&self) -> bool {
        self.available
    }

    fn mark_available(&mut self, state: bool) {
        self.available = state;
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    #[allow(dead_code)]
    pub fn contains(&self, addr: &usize) -> bool {
        (self.base..self.base + self.count * self.step).contains(addr) && (addr - self.base) % self.step == 0
    }

    #[allow(dead_code)]
    pub fn split(&mut self, addr: usize) -> Option<Self> {
        let color_region = ColorMemRegion::new(self.color, addr, 1, self.step);
        MEM_REGION_BY_COLOR
            .lock()
            .get_mut(self.color)
            .unwrap()
            .push(color_region);
        if self.contains(&addr) {
            let left_count = (addr - self.base) / self.step;
            let right_count = self.count - left_count - 1;
            let right_base = addr + self.step;
            if left_count == 0 {
                self.count = right_count;
                self.base = right_base;
                None
            } else if right_count == 0 {
                self.count = left_count;
                None
            } else {
                self.count = left_count;
                Some(Self {
                    color: self.color,
                    base: right_base,
                    count: right_count,
                    step: self.step,
                    available: false,
                })
            }
        } else {
            None
        }
    }
}

static MEM_REGION_BY_COLOR: Mutex<Vec<Vec<ColorMemRegion>>> = Mutex::new(Vec::new());

pub fn mem_region_alloc_colors(size: usize, color_bitmap: usize) -> Result<Vec<ColorMemRegion>, AllocError> {
    // hold the lock until return
    let mut mem_region_by_color = MEM_REGION_BY_COLOR.lock();
    let color_bitmap = color_bitmap & ((1 << mem_region_by_color.len()) - 1);
    info!("alloc {:#x}B in colors {:#x}", size, color_bitmap);
    let count = color_bitmap.count_ones() as usize;
    if count == 0 {
        error!("no cache color provided");
        return Err(AllocError::AllocZeroPage);
    }
    let page_num = round_up(size, PAGE_SIZE) / PAGE_SIZE;

    let color2pages = {
        // init a vec, contains color -> page_num, init value is the free page num
        let mut color2pages = vec![];
        // get the color list, sum free space in these colors
        let mut free_pages = 0;
        for (color, region_list) in mem_region_by_color.iter().enumerate() {
            if color_bitmap & (1 << color) != 0 {
                let color_free = region_list
                    .iter()
                    .filter(|region| region.is_available())
                    .map(|region| region.count)
                    .sum();
                free_pages += color_free;
                // here, we only use color and free to record a color's free page num
                color2pages.push(ColorMemRegion::new(color, 0, color_free, 0));
            } else if color_bitmap < (1 << color) {
                break;
            }
        }
        // if free pages not satisfy, return error
        if free_pages < page_num {
            error!("free pages not satisfy");
            return Err(AllocError::OutOfFrame(page_num));
        }

        fn sort_color_list(color2pages: &mut [ColorMemRegion]) {
            // free pages ascending order (small->large)
            // if equals, color ascending order
            color2pages.sort_by(|a, b| {
                if a.count.ne(&b.count) {
                    a.count.cmp(&b.count)
                } else {
                    a.color.cmp(&b.color)
                }
            });
        }

        sort_color_list(&mut color2pages);
        // determine to alloc how many pages in a color
        // **Greedy**, because color2pages ascending order by free pages
        let mut remaining_pages = page_num;
        for (i, region) in color2pages.iter_mut().enumerate() {
            let color_size = remaining_pages / (count - i);
            region.count = usize::min(region.count, color_size);
            remaining_pages -= region.count;
        }
        debug_assert_eq!(remaining_pages, 0);
        sort_color_list(&mut color2pages);
        color2pages
    };
    let mut vm_regions: Vec<ColorMemRegion> = vec![];

    for region in color2pages.iter() {
        let color = region.color;
        let size = region.count;
        let color_region_list = mem_region_by_color.get_mut(color).unwrap();

        let mut tmp = vec![];
        for exist_region in color_region_list.iter_mut() {
            if exist_region.is_available() && exist_region.count >= size {
                exist_region.mark_available(false);
                // if still space remains
                if exist_region.count > size {
                    // add to the end of the color region list
                    tmp.push(ColorMemRegion::new(
                        color,
                        exist_region.base + size * exist_region.step,
                        exist_region.count - size,
                        exist_region.step,
                    ));
                    exist_region.count = size;
                }
                vm_regions.push(exist_region.clone());
                break;
            }
        }
        color_region_list.append(&mut tmp);
    }

    Ok(vm_regions)
}

pub fn mem_color_region_free(vm_region: &ColorMemRegion) {
    info!(
        "free {:#x}b from {:#x} in color {:#04x}",
        vm_region.count * PAGE_SIZE,
        vm_region.base,
        vm_region.color,
    );
    let mut mem_region_by_color = MEM_REGION_BY_COLOR.lock();
    let color_region_list = mem_region_by_color.get_mut(vm_region.color).unwrap();
    // free mem region
    let mut free_idx = None;
    for (idx, exist_region) in color_region_list.iter_mut().enumerate() {
        if exist_region.base == vm_region.base && !exist_region.is_available() {
            exist_region.mark_available(true);
            free_idx = Some(idx);
            break;
        }
    }
    // merge
    while let Some(merge_idx) = free_idx {
        free_idx = None;
        let tmp = color_region_list.get(merge_idx).unwrap().clone();
        for (idx, exist_region) in color_region_list.iter_mut().enumerate() {
            if exist_region.is_available() {
                if exist_region.left_neighbor(&tmp) {
                    exist_region.count += tmp.count;
                    free_idx = Some(if idx < merge_idx { idx } else { idx - 1 });
                    color_region_list.remove(merge_idx);
                    break;
                } else if tmp.left_neighbor(exist_region) {
                    let count = exist_region.count;
                    let mut_tmp = color_region_list.get_mut(merge_idx).unwrap();
                    mut_tmp.count += count;
                    free_idx = Some(if merge_idx < idx { merge_idx } else { merge_idx - 1 });
                    color_region_list.remove(idx);
                    break;
                }
            }
        }
    }
}

fn init_hypervisor_colors(colors: Vec<usize>) {
    HYPERVISOR_COLORS.call_once(|| colors);
}

#[allow(dead_code)]
pub fn get_llc_size() -> usize {
    let cpu_cache_info = CPU_CACHE.get().unwrap();
    let last_level = cpu_cache_info.min_share_level;
    cpu_cache_info.info_list[last_level - 1].size()
}

fn mem_region_init_by_colors() {
    if PLAT_DESC.mem_desc.regions.is_empty() {
        panic!("Platform Vm Memory Regions Overrun!");
    }

    let cpu_cache_info = CPU_CACHE.get().unwrap();
    let last_level = cpu_cache_info.min_share_level;
    let num_colors = cpu_cache_info.info_list[last_level - 1].num_colors();

    let hypervisor_colors = if cfg!(feature = "self-coloring") {
        (0..num_colors / 2).collect()
    } else {
        (0..num_colors).collect()
    };
    info!("Hypervisor will locate in colors {:?}", hypervisor_colors);
    init_hypervisor_colors(hypervisor_colors);

    if num_colors > usize::BITS as usize {
        panic!("Too many colors ({}) in L{}", last_level, num_colors);
    }

    let mut mem_region_by_color = MEM_REGION_BY_COLOR.lock();
    for _ in 0..num_colors {
        mem_region_by_color.push(Vec::<ColorMemRegion>::new());
    }

    let step = num_colors * PAGE_SIZE;

    for (i, range) in PLAT_DESC.mem_desc.regions.iter().enumerate() {
        let (plat_mem_region_base, plat_mem_region_size) = {
            if range.contains(&(_image_end as usize)) {
                let start = round_up(_image_end as usize, step);
                let size = range.end - start;
                (start, size)
            } else {
                (range.start, range.end)
            }
        };
        if plat_mem_region_size == 0 {
            warn!("PLAT_DESC.mem_desc.regions[{}] is empty.", i);
            continue;
        }
        // NOTE: `plat_mem_region_base` might not align to `step`
        let color_mask = (num_colors - 1) << PAGE_SHIFT;
        let base_color = (plat_mem_region_base & color_mask) >> PAGE_SHIFT;
        debug!("region[{i}] {plat_mem_region_base:#x} base color {base_color:#x}");
        for color in 0..num_colors {
            let base = if color >= base_color {
                plat_mem_region_base & (!color_mask)
            } else {
                round_up(plat_mem_region_base, step)
            } | (color << PAGE_SHIFT);
            let count = (plat_mem_region_size - (base - plat_mem_region_base) + step - 1) / step;
            if count > 0 {
                let region = ColorMemRegion::new(color, base, count, step);
                mem_region_by_color.get_mut(color).unwrap().push(region);
            }
        }
    }

    debug!("mem_vm_region_init_by_colors:");
    for (color, color_region_list) in mem_region_by_color.iter().enumerate() {
        debug!("Color {:#04x}: {:x?}", color, color_region_list);
    }
}

pub fn count_missing_num(regions: &[ColorMemRegion]) -> Vec<usize> {
    let mut list = vec![0; regions.first().unwrap().count];
    // enumerate then skip the first one
    for (i, region) in regions.iter().enumerate().skip(1) {
        let prev_count = regions.get(i - 1).unwrap().count;
        for _ in prev_count..region.count {
            list.push(list.last().unwrap() + i);
        }
    }
    list
}

fn cpu_map_va2color_regions(cpu: &Cpu, cpu_va_region: RangeInclusive<usize>, color_regions: &[ColorMemRegion]) {
    let missing_list = count_missing_num(color_regions);
    for (i, region) in color_regions.iter().enumerate() {
        for j in 0..region.count {
            let missing_num = missing_list.get(j).unwrap();
            let page_idx = i + j * color_regions.len() - missing_num;
            let va = cpu_va_region.start() + page_idx * PAGE_SIZE;
            let pa = region.base + j * region.step;
            cpu.pt().pt_map_range(va, PAGE_SIZE, pa, PTE_S1_NORMAL, false);
        }
    }
}

fn space_remapping<T: Sized>(src: *const T, len: usize, color_bitmap: usize) -> (&'static mut T, Vec<ColorMemRegion>) {
    // alloc mem pages
    let color_regions = mem_region_alloc_colors(len, color_bitmap).expect("mem_region_alloc_colors() error");
    debug!("space_remapping: color_regions {:x?}", color_regions);
    // alloc va space
    let va_pages = vpage_alloc(len, None).expect("vpage_alloc");
    debug!("space_remapping: va pages {:?}", va_pages);
    let dest_va = va_pages.start().start_address().as_mut_ptr();
    let range = va_pages.as_range_incluesive();
    debug!("space_remapping: dest va {:x?}", range);
    // map va with pa
    cpu_map_va2color_regions(current_cpu(), range, &color_regions);
    // copy src to va
    unsafe {
        core::ptr::copy_nonoverlapping(src as *const u8, dest_va, len);
    }
    Arch::dcache_clean_flush(dest_va as usize, len);
    (unsafe { &mut *(dest_va as *mut T) }, color_regions)
}

fn device_mapping(cpu: &Cpu) {
    let device_regions = crate::board::Platform::device_regions();
    for device in device_regions.iter() {
        assert_eq!(device.start % 0x20_0000, 0);
        assert_eq!(device.len() % 0x20_0000, 0);
        cpu.pt()
            .pt_map_range(device.start, device.len(), device.start, PTE_S1_DEVICE, true);
    }
    for i in 0..crate::arch::PLATFORM_PHYSICAL_LIMIT_GB {
        let pa = i << crate::arch::LVL1_SHIFT;
        let hva = crate::arch::Address::pa2hva(pa);
        cpu.pt()
            .pt_map_range(hva, 1 << crate::arch::LVL1_SHIFT, pa, PTE_S1_NORMAL, true);
    }
}

#[inline]
unsafe fn relocate_space(cpu_new: &Cpu, root_pt: usize) {
    // NOTE: do nothing complex (means need stack, heap or any global variables)
    // for example, `println!()`, `info!()` is not allowed here
    // because it may cause unpredictable problems

    let current_sp = crate::arch::Arch::current_stack_pointer();
    let length = current_cpu().stack_top() - current_sp;

    let dst_sp = cpu_new.stack_top() - length;
    core::ptr::copy_nonoverlapping(current_sp as *const u8, dst_sp as *mut u8, length);

    crate::arch::Arch::invalid_hypervisor_all();
    // switch to page table
    crate::arch::Arch::install_self_page_table(root_pt);
    crate::arch::Arch::invalid_hypervisor_all();
}

#[allow(clippy::forget_non_drop)]
pub fn hypervisor_self_coloring() {
    let cpu_cache_info = CPU_CACHE.get().unwrap();
    let last_level = cpu_cache_info.min_share_level;
    let num_colors = cpu_cache_info.info_list[last_level - 1].num_colors();

    let mut self_color_bitmap = 0;
    for x in HYPERVISOR_COLORS.get().unwrap().iter() {
        self_color_bitmap |= 1 << x;
    }

    if self_color_bitmap == 0 || ((self_color_bitmap & ((1 << num_colors) - 1)) == ((1 << num_colors) - 1)) {
        enlarge_heap(self_color_bitmap);
        return;
    }

    // Copy the CPU space into a colored region
    let cpu = current_cpu();
    let (cpu_new, cpu_pa_regions) = space_remapping(cpu, size_of::<Cpu>(), self_color_bitmap);
    // init the page table in cpu_new
    cpu_new.cpu_pt.lvl1.fill(0);
    cpu_new.cpu_pt.lvl2.fill(0);
    cpu_new.cpu_pt.lvl3.fill(0);
    match current_cpu().pt().ipa2pa(cpu_new.cpu_pt.lvl1.as_ptr() as usize) {
        Some(directory) => cpu_new.reset_pt(directory),
        None => panic!(
            "Invalid va {:#x} when rewrite cpu_new page table",
            cpu_new.cpu_pt.lvl1.as_ptr() as usize
        ),
    }
    let cpu_va_range = CPU_BANKED_ADDRESS..=CPU_BANKED_ADDRESS + size_of::<Cpu>() - 1;
    cpu_map_va2color_regions(cpu_new, cpu_va_range, &cpu_pa_regions);
    device_mapping(cpu_new);
    // never drop the heap physical memory
    core::mem::forget(cpu_pa_regions);
    debug!("new cpu {} page table init OK", cpu_new.id);

    static NEW_IMAGE_SHARED_PTE: Once<usize> = Once::new();
    static NEW_IMAGE_START: Once<usize> = Once::new();
    let image_start = _image_start as usize;
    let image_size = _image_end as usize - image_start;
    // Copy the hypervisor image
    if current_cpu().id == CPU_MASTER {
        debug!("image size: {image_size:#x}");
        let image = unsafe { core::slice::from_raw_parts(image_start as *const u8, image_size) };
        let (new_start, pa_regions) = space_remapping(image.as_ptr(), image.len(), self_color_bitmap);
        NEW_IMAGE_START.call_once(|| new_start as *const _ as usize);

        let image_range = image_start..=image_start + image_size - 1;
        cpu_map_va2color_regions(cpu_new, image_range, &pa_regions);
        // never drop the physical memory
        core::mem::forget(pa_regions);

        match cpu_new.pt().get_pte(image_start, 1) {
            Some(pte) => NEW_IMAGE_SHARED_PTE.call_once(|| pte),
            None => panic!("core {} get pte error, va {:#x}", current_cpu().id, image_start),
        };
    } else {
        // wait until master cpu finish image copy
        NEW_IMAGE_SHARED_PTE.wait();

        let pte = *NEW_IMAGE_SHARED_PTE.get().unwrap();
        cpu_new.pt().set_pte(image_start, 1, pte);
        debug!("core {} set va {image_start:#x} pte {pte:#x}", current_cpu().id);
    }
    barrier();

    // copy again: for heap space in bss segment
    if current_cpu().id == CPU_MASTER {
        unsafe {
            core::ptr::copy_nonoverlapping(
                image_start as *const u8,
                *NEW_IMAGE_START.get().unwrap() as *mut u8,
                image_size,
            )
        };
    }
    barrier();
    // NOTE: Now, don't use heap, stack or any global variables

    unsafe {
        relocate_space(cpu_new, cpu_new.pt().base_pa());
    }
    // cache invalidate
    Arch::dcache_clean_flush(image_start, image_size);
    Arch::dcache_clean_flush(CPU_BANKED_ADDRESS, size_of::<Cpu>());

    /*
        The barrier object is in an inconsistent state, because we use barrier after image copy,
        and they need to be re-initialized before they get used again,
        so CPUs need a way to communicate between themselves without an explicit barrier.
    */
    static BARRIER_RESET: Once<()> = Once::new();
    if current_cpu().id == CPU_MASTER {
        reset_barrier();
        BARRIER_RESET.call_once(|| ());
    } else {
        BARRIER_RESET.wait();
    }
    enlarge_heap(self_color_bitmap);
    info!("=== core {} finish self_coloring ===", current_cpu().id);
}

#[allow(clippy::forget_non_drop)]
fn enlarge_heap(self_color_bitmap: usize) {
    // Core 0 apply for va and pa pages
    static HEAP_PAGES: Once<AllocatedPages> = Once::new();
    static HEAP_SHARED_PTE: Once<usize> = Once::new();
    const HEAP_SIZE: usize = 32 * (1 << 20); // 32 MB
    if current_cpu().id == CPU_MASTER {
        match vpage_alloc(HEAP_SIZE, Some(1 << 20)) {
            Ok(pages) => HEAP_PAGES.call_once(|| pages),
            Err(err) => panic!("vpage_alloc failed {err:?}"),
        };
        let heap_color_regions = match mem_region_alloc_colors(HEAP_SIZE, self_color_bitmap) {
            Ok(color_regions) => {
                debug!("HEAP_COLOR_REGIONS: {color_regions:x?}");
                color_regions
            }
            Err(_) => panic!("mem_region_alloc_colors failed"),
        };
        let heap_range = HEAP_PAGES.get().unwrap().as_range_incluesive();
        cpu_map_va2color_regions(current_cpu(), heap_range.clone(), &heap_color_regions);
        heap_expansion(heap_range.clone());
        // never drop the heap physical memory
        core::mem::forget(heap_color_regions);

        match current_cpu().pt().get_pte(*heap_range.start(), 1) {
            Some(pte) => HEAP_SHARED_PTE.call_once(|| pte),
            None => panic!("core {} get pte error, va {:#x}", current_cpu().id, heap_range.start()),
        };
    } else {
        HEAP_PAGES.wait();
        HEAP_SHARED_PTE.wait();

        let pte = *HEAP_SHARED_PTE.get().unwrap();
        let heap_range = HEAP_PAGES.get().unwrap().as_range_incluesive();
        current_cpu().pt().set_pte(*heap_range.start(), 1, pte);
        debug!(
            "core {} set va {:#x} pte {pte:#x}",
            current_cpu().id,
            heap_range.start()
        );
    }
    barrier();
}
