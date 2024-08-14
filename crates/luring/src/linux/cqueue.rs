pub trait Entry: io_uring::cqueue::EntryMarker + Unpin + 'static {
    fn user_data(&self) -> u64;
    fn result(&self) -> i32;
    fn flags(&self) -> u32;
}

impl Entry for io_uring::cqueue::Entry {
    #[inline(always)]
    fn user_data(&self) -> u64 {
        self.user_data()
    }

    #[inline(always)]
    fn result(&self) -> i32 {
        self.result()
    }

    #[inline(always)]
    fn flags(&self) -> u32 {
        self.flags()
    }
}

impl Entry for io_uring::cqueue::Entry32 {
    #[inline(always)]
    fn user_data(&self) -> u64 {
        self.user_data()
    }

    #[inline(always)]
    fn result(&self) -> i32 {
        self.result()
    }

    #[inline(always)]
    fn flags(&self) -> u32 {
        self.flags()
    }
}
