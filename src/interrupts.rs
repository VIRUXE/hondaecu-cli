// OKI MSM66207 Hardware Interrupt Dispatcher

use crate::cpu::Cpu;
use crate::bus::{Bus, SFR_IE, SFR_IRQ};

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
        let current_irq = bus.read_data_u16(SFR_IRQ) | extra_irq;
        let ie = bus.read_data_u16(SFR_IE);

        if !cpu.ie || (current_irq & ie) == 0 {
            return false;
        }

        let pending = current_irq & ie;

        // Vector priority check
        let (bit, isr_vector) = if (pending & (1 << 5)) != 0 {
            (5, VECTOR_INT0) // High priority distributor CKP pulse
        } else if (pending & (1 << 9)) != 0 {
            (9, VECTOR_INT1) // High priority distributor TDC pulse
        } else if (pending & (1 << 2)) != 0 {
            (2, VECTOR_TM0)  // Timer 0 Injector PWM
        } else if (pending & (1 << 3)) != 0 {
            (3, VECTOR_TM1)  // Timer 1 Periodic ADC
        } else if (pending & (1 << 4)) != 0 {
            (4, VECTOR_TM2)  // Timer 2 Injection Scheduler
        } else if (pending & (1 << 6)) != 0 {
            (6, VECTOR_SERIAL_RX) // Serial RX Datalogging
        } else if (pending & (1 << 7)) != 0 {
            (7, VECTOR_TM3)  // Timer 3 Aux
        } else {
            return false;
        };

        // Clear serviced IRQ bit
        let new_irq = current_irq & !(1 << bit);
        bus.write_data_u16(SFR_IRQ, new_irq);

        // Save PSW and return PC on System Stack
        let psw = cpu.psw_u16();
        cpu.ssp = cpu.ssp.wrapping_sub(2);
        bus.write_data_u16(cpu.ssp, psw);

        cpu.ssp = cpu.ssp.wrapping_sub(2);
        bus.write_data_u16(cpu.ssp, cpu.pc);

        // Disable global interrupts during ISR entry
        cpu.ie = false;

        // Jump to target ISR vector address
        cpu.pc = isr_vector;
        true
    }
}
