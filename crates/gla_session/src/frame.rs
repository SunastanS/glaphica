#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameBudget {
    max_dabs: u32,
    accepted: u32,
}

impl FrameBudget {
    pub fn new(max_dabs: u32) -> Self {
        Self {
            max_dabs,
            accepted: 0,
        }
    }

    pub fn try_accept_dab(&mut self) -> bool {
        if self.accepted >= self.max_dabs {
            return false;
        }
        self.accepted += 1;
        true
    }

    pub fn accepted(&self) -> u32 {
        self.accepted
    }
}

#[cfg(test)]
mod tests {
    use super::FrameBudget;

    #[test]
    fn frame_budget_accepts_until_dab_budget_is_exhausted() {
        let mut budget = FrameBudget::new(2);

        assert!(budget.try_accept_dab());
        assert!(budget.try_accept_dab());
        assert!(!budget.try_accept_dab());
        assert_eq!(budget.accepted(), 2);
    }
}
