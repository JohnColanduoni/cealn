#[derive(Clone, Debug)]
pub struct Options {
    pub entries: u32,
    pub cq_size: Option<u32>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            // FIXME: don't pull this out of our ass
            entries: 512,
            cq_size: None,
        }
    }
}
