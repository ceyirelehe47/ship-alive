//! Minimal production chain: asteroid ore → fabricator → machinery parts.
//!
//! A fabricator is a built 2x2 machine. The player gives it an order
//! (`Produce N` or `Repeat`); auto-logistics hauls ore into its input buffer;
//! when inputs are satisfied a crew member walks over and *operates* the
//! machine for the recipe duration — nothing happens without a worker. Output
//! piles up in the machine's output buffer until haulers move it to storage,
//! so a full output blocks further cycles (visible machine state).

use crate::items::ItemKind;
use bevy::prelude::*;

/// The single Slice 1 recipe.
pub struct Recipe {
    pub in_kind: ItemKind,
    pub in_qty: u32,
    pub out_kind: ItemKind,
    pub out_qty: u32,
    pub work_secs: f32,
}

pub const RECIPE: Recipe = Recipe {
    in_kind: ItemKind::Ore,
    in_qty: 2,
    out_kind: ItemKind::Part,
    out_qty: 1,
    work_secs: 6.0,
};

/// One production order: `batches` more runs, or endless when `repeat`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Order {
    pub batches: u32,
    pub repeat: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MachineState {
    NoOrder,
    WaitingInput,
    WaitingWorker,
    Working,
    OutputBlocked,
}

impl MachineState {
    pub fn label(&self) -> &'static str {
        match self {
            MachineState::NoOrder => "Idle — no order",
            MachineState::WaitingInput => "Waiting for input",
            MachineState::WaitingWorker => "Waiting for worker",
            MachineState::Working => "Working",
            MachineState::OutputBlocked => "Output blocked — haul parts away",
        }
    }

    pub fn blocked(&self) -> bool {
        matches!(self, MachineState::OutputBlocked)
    }
}

#[derive(Component, Debug)]
pub struct Fabricator {
    /// Input buffer counts by `ItemKind::index`.
    pub input: [u32; 3],
    /// Output buffer counts by `ItemKind::index`.
    pub output: [u32; 3],
    pub order: Option<Order>,
    /// True while a crew member is operating the current cycle.
    pub active: bool,
    /// 0..1 progress of the current cycle.
    pub progress: f32,
}

impl Default for Fabricator {
    fn default() -> Self {
        Self {
            input: [0, 0, 0],
            output: [0, 0, 0],
            order: None,
            active: false,
            progress: 0.0,
        }
    }
}

impl Fabricator {
    pub const INPUT_CAP: u32 = 8;
    pub const OUTPUT_CAP: u32 = 6;

    pub fn state(&self) -> MachineState {
        let Some(order) = &self.order else {
            return MachineState::NoOrder;
        };
        let _ = order;
        if self.output[RECIPE.out_kind.index()] + RECIPE.out_qty > Self::OUTPUT_CAP {
            return MachineState::OutputBlocked;
        }
        if self.input[RECIPE.in_kind.index()] < RECIPE.in_qty {
            return MachineState::WaitingInput;
        }
        if self.active {
            MachineState::Working
        } else {
            MachineState::WaitingWorker
        }
    }

    /// Can a worker start a cycle right now?
    pub fn ready_to_work(&self) -> bool {
        self.state() == MachineState::WaitingWorker
    }

    /// How many more units of recipe input the logistics system should deliver
    /// (`inbound` = units already reserved by haulers heading here).
    pub fn input_want(&self, inbound: u32) -> u32 {
        if self.order.is_none() || self.state().blocked() {
            return 0;
        }
        let need_total = match self.order {
            Some(o) if o.repeat => Self::INPUT_CAP,
            Some(o) => (o.batches * RECIPE.in_qty).min(Self::INPUT_CAP),
            None => 0,
        };
        need_total
            .saturating_sub(self.input[RECIPE.in_kind.index()])
            .saturating_sub(inbound)
    }

    /// Complete one production cycle (inputs are consumed at the end, so a
    /// canceled cycle never destroys material). Returns the produced kind.
    pub fn finish_cycle(&mut self) -> ItemKind {
        self.input[RECIPE.in_kind.index()] =
            self.input[RECIPE.in_kind.index()].saturating_sub(RECIPE.in_qty);
        self.output[RECIPE.out_kind.index()] += RECIPE.out_qty;
        match &mut self.order {
            Some(o) if !o.repeat => {
                o.batches = o.batches.saturating_sub(1);
                if o.batches == 0 {
                    self.order = None;
                }
            }
            _ => {}
        }
        self.active = false;
        self.progress = 0.0;
        RECIPE.out_kind
    }

    /// Abort the current cycle (worker left): no material is consumed.
    pub fn abort_cycle(&mut self) {
        self.active = false;
        self.progress = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fab_with_order(batches: u32, ore: u32) -> Fabricator {
        let mut f = Fabricator {
            order: Some(Order {
                batches,
                repeat: false,
            }),
            ..Fabricator::default()
        };
        f.input[ItemKind::Ore.index()] = ore;
        f
    }

    #[test]
    fn no_input_no_work() {
        let mut f = fab_with_order(2, 1);
        assert_eq!(f.state(), MachineState::WaitingInput);
        assert!(!f.ready_to_work());
        f.input[ItemKind::Ore.index()] = 2;
        assert_eq!(f.state(), MachineState::WaitingWorker);
        assert!(f.ready_to_work());
    }

    #[test]
    fn cycle_consumes_input_and_counts_down() {
        let mut f = fab_with_order(2, 2);
        f.active = true;
        assert_eq!(f.state(), MachineState::Working);
        let out = f.finish_cycle();
        assert_eq!(out, ItemKind::Part);
        assert_eq!(f.input[ItemKind::Ore.index()], 0);
        assert_eq!(f.output[ItemKind::Part.index()], 1);
        assert_eq!(f.order.unwrap().batches, 1);
        assert!(!f.active);
    }

    #[test]
    fn output_blocked_stops_cycles() {
        let mut f = fab_with_order(3, 4);
        f.output[ItemKind::Part.index()] = Fabricator::OUTPUT_CAP - RECIPE.out_qty + 1;
        assert_eq!(f.state(), MachineState::OutputBlocked);
        assert!(!f.ready_to_work());
        assert_eq!(f.input_want(0), 0, "no more supply while blocked");
    }

    #[test]
    fn repeat_mode_keeps_order_and_caps_supply() {
        let mut f = Fabricator {
            order: Some(Order {
                batches: 0,
                repeat: true,
            }),
            ..Fabricator::default()
        };
        f.input[ItemKind::Ore.index()] = 2;
        assert_eq!(f.input_want(0), Fabricator::INPUT_CAP - 2);
        f.finish_cycle();
        assert!(f.order.unwrap().repeat);
    }

    #[test]
    fn aborting_a_cycle_keeps_material() {
        let mut f = fab_with_order(1, 2);
        f.active = true;
        f.abort_cycle();
        assert_eq!(f.input[ItemKind::Ore.index()], 2);
        assert_eq!(f.state(), MachineState::WaitingWorker);
    }

    #[test]
    fn input_want_respects_batches_and_inbound() {
        let f = fab_with_order(2, 1);
        assert_eq!(f.input_want(0), 3); // 2 batches * 2 ore - 1 stored
        assert_eq!(f.input_want(2), 1);
        assert_eq!(f.input_want(9), 0);
    }
}
