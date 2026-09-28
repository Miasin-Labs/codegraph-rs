use widgets_core::Named;

pub struct Gear {
    teeth: u32,
}

impl Gear {
    pub fn new(teeth: u32) -> Gear {
        Gear { teeth }
    }

    pub fn teeth(&self) -> u32 {
        self.teeth
    }
}

impl Named for Gear {
    fn name(&self) -> String {
        format!("gear{}", self.teeth)
    }
}
