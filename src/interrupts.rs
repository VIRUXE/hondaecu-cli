// OKI MSM66207 Hardware Interrupt Dispatcher

use crate::bus::{Bus, SFR_IE, SFR_IRQ};
use crate::cpu::Cpu;

pub const VECTOR_RESET: u16 = 0x0000;
pub const VECTOR_BRK: u16 = 0x0002;
pub const VECTOR_WATCHDOG: u16 = 0x0004;
pub const VECTOR_NMI: u16 = 0x0006;
pub const VECTOR_IRQ_BASE: u16 = 0x0008;

pub struct InterruptController;

impl InterruptController {
    /// Check if pending IRQs match enabled IE flags and dispatch ISR call
    pub fn handle_pending_interrupts(cpu: &mut Cpu, bus: &mut Bus, extra_irq: u16) -> bool {
        // Peripheral edges set IRQ flags even while an ISR is active or the
        // corresponding IE bit is clear. Latch them before deciding whether
        // the CPU can accept another interrupt.
        let current_irq = bus.read_data_u16(SFR_IRQ) | extra_irq;
        bus.write_data_u16(SFR_IRQ, current_irq);
        let ie = bus.read_data_u16(SFR_IE);

        // PSW.8 is MIE, the global maskable-interrupt enable flag. The full
        // MSM66201/207 user's manual specifies that interrupt entry saves PSW
        // and then clears MIE; firmware may set it again inside an ISR when it
        // deliberately permits nesting.
        if !cpu.mie() || (current_irq & ie) == 0 {
            return false;
        }

        let pending = current_irq & ie;
        let bit = pending.trailing_zeros() as u16;
        // The 16 IRQ/IE bits map directly to 16 little-endian vector words.
        // Reading the ROM table also makes this work for ROM variants whose
        // handlers are linked at different addresses.
        let isr_vector = bus.read_code_u16(VECTOR_IRQ_BASE + bit * 2);

        // Clear serviced IRQ bit
        let new_irq = current_irq & !(1 << bit);
        bus.write_data_u16(SFR_IRQ, new_irq);

        // Hardware interrupt frame, MSM66201 RTI manual p. 3-126:
        // PC, A, LRB and PSW are saved using the system stack's
        // store-then-decrement convention. RTI restores them in reverse order.
        let psw = cpu.psw_u16();
        bus.write_data_u16(cpu.ssp, cpu.pc);
        cpu.ssp = cpu.ssp.wrapping_sub(2);
        bus.write_data_u16(cpu.ssp, cpu.a);
        cpu.ssp = cpu.ssp.wrapping_sub(2);
        bus.write_data_u16(cpu.ssp, cpu.lrb);
        cpu.ssp = cpu.ssp.wrapping_sub(2);
        bus.write_data_u16(cpu.ssp, psw);
        cpu.ssp = cpu.ssp.wrapping_sub(2);

        // Maskable interrupt entry clears MIEF after saving the old PSW.
        cpu.set_mie(false);

        // Jump to target ISR vector address
        cpu.pc = isr_vector;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{IRQ_INT0, IRQ_TM0};
    use crate::exec;

    #[test]
    fn dispatch_uses_the_rom_vector_for_the_pending_bit() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new();
        cpu.pc = 0x3456;
        cpu.a = 0xABCD;
        cpu.lrb = 0x0041;
        cpu.ssp = 0x047E;
        cpu.set_mie(true);
        let saved_psw = cpu.psw_u16();
        bus.rom[(0x0008 + IRQ_TM0 as usize * 2)..(0x000A + IRQ_TM0 as usize * 2)]
            .copy_from_slice(&[0xBC, 0x2A]);
        bus.rom[0x2ABC] = 0x02; // RTI
        bus.write_data_u16(SFR_IE, 1 << IRQ_TM0);

        assert!(InterruptController::handle_pending_interrupts(
            &mut cpu,
            &mut bus,
            1 << IRQ_TM0
        ));
        assert_eq!(cpu.pc, 0x2ABC);
        assert_eq!(bus.read_data_u16(SFR_IRQ) & (1 << IRQ_TM0), 0);
        assert!(!cpu.mie());
        assert_eq!(cpu.ssp, 0x0476);
        assert_eq!(bus.read_data_u16(0x0478), saved_psw);
        assert_eq!(bus.read_data_u16(0x047A), 0x0041);
        assert_eq!(bus.read_data_u16(0x047C), 0xABCD);
        assert_eq!(bus.read_data_u16(0x047E), 0x3456);

        // An ISR may freely change all hardware-saved registers. RTI must
        // restore the interrupted context and the saved MIE state.
        cpu.a = 0x1111;
        cpu.lrb = 0x0081;
        cpu.set_psw_u16(0);
        exec::step(&mut cpu, &mut bus).expect("RTI executes");
        assert_eq!(cpu.pc, 0x3456);
        assert_eq!(cpu.a, 0xABCD);
        assert_eq!(cpu.lrb, 0x0041);
        assert_eq!(cpu.psw_u16(), saved_psw);
        assert_eq!(cpu.ssp, 0x047E);
    }

    #[test]
    fn pending_edges_remain_latched_while_interrupts_are_blocked() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new();
        cpu.set_mie(false);

        assert!(!InterruptController::handle_pending_interrupts(
            &mut cpu,
            &mut bus,
            1 << IRQ_INT0
        ));
        assert_ne!(bus.read_data_u16(SFR_IRQ) & (1 << IRQ_INT0), 0);
    }
}
