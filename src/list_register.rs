pub struct ListRegister {
    lr: u32,
}

impl ListRegister {
    pub fn new(value: u32) -> Self {
        Self { lr: value }
    }

    pub fn set(mut self, value: u32) {
        self.lr = value.into();
    }

    pub fn get(&self) -> u32 {
        self.lr
    }
}
