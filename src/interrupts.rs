// OKI MSM66207 Hardware Interrupt Dispatcher

use crate::cpu::Cpu;
use crate::bus::{Bus, EXTERNAL_DATA_START, SFR_IE, SFR_IRQ};

/// Machine cycles the hardware takes to transfer to a maskable interrupt
/// routine: saving PC, A, LRB and PSW, clearing the request and MIE, and
/// loading the vector.
///
/// **Derived, not quoted.** The available MSM66201/66207 documents give no
/// figure (user's manual ch. 17, pp. 163-170; the instruction manual lists
/// instructions only). Seventeen is what the instruction manual's own counts
/// imply: RTI undoes exactly this frame in 15 cycles, 2 of them its opcode
/// fetch, and fetching a vector costs 4 (VCAL is 11 where RT is 7), so
/// 15 - 2 + 4 = 17. The manuals of the successor nX-8/500S core, which saves
/// the same four registers, state the same relation outright: 14 cycles to
/// enter, 12 for RTI, again two apart (MSM66573 Family User's Manual, p. 16-9).
pub const INTERRUPT_ENTRY_CYCLES: u32 = 17;

/// Extra cycles when the system stack is in external memory: four per word
/// pushed, the amount by which the manual's EXT count for RTI exceeds its INT
/// count (31 against 15).
pub const INTERRUPT_ENTRY_EXTERNAL_STACK_CYCLES: u32 = 16;

pub const VECTOR_RESET: u16    = 0x0000;
pub const VECTOR_NMI: u16      = 0x003C;
pub const VECTOR_TM0: u16      = 0x0086;
pub const VECTOR_TM1: u16      = 0x0114;
pub const VECTOR_TM2: u16      = 0x014D;
pub const VECTOR_INT0: u16     = 0x01D3;
pub const VECTOR_SERIAL_RX: u16= 0x01FD;
pub const VECTOR_TM2_OVF: u16  = 0x02DD;
pub const VECTOR_TM3: u16      = 0x02FB;
pub const VECTOR_PWM_IACV: u16 = 0x031E;
pub const VECTOR_INT1: u16     = 0x037B;

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

        // PSW.8 is MIE, the global maskable-interrupt enable. The full
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
        let isr_vector = bus.read_code_u16(0x0008 + bit * 2);

        // Clear serviced IRQ bit
        let new_irq = current_irq & !(1 << bit);
        bus.write_data_u16(SFR_IRQ, new_irq);

        // Hardware interrupt frame, MSM66201 RTI manual p. 3-126:
        // PC, A, LRB and PSW are saved using the system stack's
        // store-then-decrement convention. RTI restores them in reverse order.
        let stack_is_external = cpu.ssp >= EXTERNAL_DATA_START;
        let psw = cpu.psw_u16();
        bus.write_data_u16(cpu.ssp, cpu.pc);
        cpu.ssp = cpu.ssp.wrapping_sub(2);
        bus.write_data_u16(cpu.ssp, cpu.a);
        cpu.ssp = cpu.ssp.wrapping_sub(2);
        bus.write_data_u16(cpu.ssp, cpu.lrb);
        cpu.ssp = cpu.ssp.wrapping_sub(2);
        bus.write_data_u16(cpu.ssp, psw);
        cpu.ssp = cpu.ssp.wrapping_sub(2);

        // Maskable interrupt entry clears MIE after saving the old PSW.
        cpu.set_mie(false);

        // Jump to target ISR vector address
        cpu.pc = isr_vector;

        // The transfer takes time, and the peripherals keep running through
        // it. A request they raise meanwhile stays pending for the next call.
        let cycles = if stack_is_external {
            INTERRUPT_ENTRY_CYCLES + INTERRUPT_ENTRY_EXTERNAL_STACK_CYCLES
        } else {
            INTERRUPT_ENTRY_CYCLES
        };
        cpu.cycles += cycles as u64;
        let raised = bus.tick_timers(cycles);
        if raised != 0 {
            let irq = bus.read_data_u16(SFR_IRQ);
            bus.write_data_u16(SFR_IRQ, irq | raised);
        }
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

    /// CPU and bus with INT0 enabled, its vector at 2ABCh and MIE set.
    fn ready_for_int0(ssp: u16) -> (Cpu, Bus) {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new();
        cpu.pc = 0x3456;
        cpu.ssp = ssp;
        cpu.set_mie(true);
        bus.rom[0x0008..0x000A].copy_from_slice(&[0xBC, 0x2A]);
        bus.write_data_u16(SFR_IE, 1 << IRQ_INT0);
        (cpu, bus)
    }

    #[test]
    fn taking_an_interrupt_costs_seventeen_machine_cycles() {
        let (mut cpu, mut bus) = ready_for_int0(0x047E);

        assert!(InterruptController::handle_pending_interrupts(
            &mut cpu,
            &mut bus,
            1 << IRQ_INT0
        ));

        assert_eq!(cpu.pc, 0x2ABC);
        assert_eq!(cpu.cycles, 17);
        assert_eq!(bus.elapsed_cycles, 17, "the peripherals run through the entry");
    }

    /// Four words go onto the stack; an external stack costs four cycles a
    /// word, as it does for RTI (15 on-chip, 31 external).
    #[test]
    fn an_external_system_stack_makes_interrupt_entry_dearer() {
        let (mut cpu, mut bus) = ready_for_int0(0xFFFE);

        assert!(InterruptController::handle_pending_interrupts(
            &mut cpu,
            &mut bus,
            1 << IRQ_INT0
        ));

        assert_eq!(cpu.cycles, 33);
        assert_eq!(bus.elapsed_cycles, 33);
    }

    #[test]
    fn an_interrupt_that_is_not_taken_costs_nothing() {
        let (mut cpu, mut bus) = ready_for_int0(0x047E);
        cpu.set_mie(false);

        assert!(!InterruptController::handle_pending_interrupts(
            &mut cpu,
            &mut bus,
            1 << IRQ_INT0
        ));

        assert_eq!(cpu.cycles, 0);
        assert_eq!(bus.elapsed_cycles, 0);
    }

    /// A timer that overflows while the hardware is saving the context must
    /// still raise its request: timer 1 here is three counts from overflow.
    #[test]
    fn a_timer_overflow_during_interrupt_entry_is_latched() {
        use crate::bus::{SFR_TCON1, SFR_TM1, SFR_TMR1, TCON_RUN, TIMER_MODE_AUTO_RELOAD};
        const IRQ_TM1_OVF: u16 = 6;

        let (mut cpu, mut bus) = ready_for_int0(0x047E);
        bus.write_data_u8(SFR_TCON1, TCON_RUN | TIMER_MODE_AUTO_RELOAD);
        bus.write_data_u16(SFR_TMR1, 0xFF00);
        bus.write_data_u16(SFR_TM1, 0xFFFE);

        assert!(InterruptController::handle_pending_interrupts(
            &mut cpu,
            &mut bus,
            1 << IRQ_INT0
        ));

        let irq = bus.read_data_u16(SFR_IRQ);
        assert_eq!(irq & (1 << IRQ_INT0), 0, "the serviced request is cleared");
        assert_ne!(irq & (1 << IRQ_TM1_OVF), 0, "the overflow is pending");
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
