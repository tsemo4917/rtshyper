use alloc::alloc;
use core::alloc::Layout;

use crate::arch::PAGE_SIZE;
use crate::kernel::{current_cpu, AllocError};

#[derive(Debug, raii::RAII)]
pub struct PageFrame {
    pub hva: usize,
    pub page_num: usize,
    pub pa: usize,
    layout: Layout,
}

#[allow(dead_code)]
impl PageFrame {
    fn new(hva: usize, page_num: usize, layout: Layout) -> Self {
        Self {
            hva,
            page_num,
            pa: current_cpu().pt().ipa2pa(hva).unwrap(),
            layout,
        }
    }

    pub fn alloc_pages(page_num: usize) -> Result<Self, AllocError> {
        if page_num == 0 {
            return Err(AllocError::AllocZeroPage);
        }
        match Layout::from_size_align(page_num * PAGE_SIZE, PAGE_SIZE) {
            Ok(layout) => {
                let hva = unsafe { alloc::alloc_zeroed(layout) };
                if hva.is_null() || hva as usize & (PAGE_SIZE - 1) != 0 {
                    panic!("alloc_pages: get wrong ptr {hva:#p}, layout = {:?}", layout);
                }
                let hva = hva as usize;
                Ok(Self::new(hva, page_num, layout))
            }
            Err(err) => {
                error!("alloc_pages: Layout error {}", err);
                Err(AllocError::OutOfFrame(page_num))
            }
        }
    }

    pub fn pa(&self) -> usize {
        self.pa
    }

    pub fn hva(&self) -> usize {
        self.hva
    }
}

impl Drop for PageFrame {
    fn drop(&mut self) {
        trace!("<<< free page frame {:#x}, {}", self.pa, self.page_num);
        unsafe { alloc::dealloc(self.hva as *mut _, self.layout) }
    }
}
