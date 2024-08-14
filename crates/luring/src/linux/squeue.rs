pub trait Entry: io_uring::squeue::EntryMarker + 'static + From<io_uring::squeue::Entry> {
    fn user_data(self, user_data: u64) -> Self;
}

impl Entry for io_uring::squeue::Entry {
    #[inline(always)]
    fn user_data(self, user_data: u64) -> Self {
        self.user_data(user_data)
    }
}

impl Entry for io_uring::squeue::Entry128 {
    #[inline(always)]
    fn user_data(self, user_data: u64) -> Self {
        self.user_data(user_data)
    }
}
