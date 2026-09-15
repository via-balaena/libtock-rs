#![no_std]

use core::cell::Cell;
use libtock_platform as platform;
use libtock_platform::allow_rw::AllowRw;
use libtock_platform::share;
use libtock_platform::subscribe::Subscribe;
use libtock_platform::AllowRo;
use libtock_platform::{DefaultConfig, ErrorCode, Syscalls};

pub struct SpiController<S: Syscalls, C: Config = DefaultConfig>(S, C);

impl<S: Syscalls, C: Config> SpiController<S, C> {
    pub fn exists() -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, spi_controller_cmd::EXISTS, 0, 0).to_result()
    }

    /// # Summary
    ///
    /// Perform an I2C write followed by a read.
    ///
    /// TODO: Add async support
    ///
    /// # Parameter
    ///
    /// * `addr`: Slave device address
    /// * `buf`: Buffer
    /// * `w_len`: Number of bytes to write from @w_buf
    /// * `r_len`: Number of bytes to read into @r_buf
    ///
    /// # Returns
    /// On success: Returns Ok(())
    /// On failure: Err(ErrorCode)
    pub fn spi_controller_write_read_sync(
        w_buf: &[u8],
        r_buf: &mut [u8],
        len: u32,
    ) -> Result<(), ErrorCode> {
        if len as usize > w_buf.len() || len as usize > r_buf.len() {
            return Err(ErrorCode::NoMem);
        }

        let called: Cell<Option<(u32, u32, u32)>> = Cell::new(None);
        share::scope::<
            (
                AllowRw<_, DRIVER_NUM, { rw_allow::READ }>,
                AllowRo<_, DRIVER_NUM, { ro_allow::WRITE }>,
                Subscribe<_, DRIVER_NUM, { subscribe::COMPLETE }>,
            ),
            _,
            _,
        >(|handle| {
            let (allow_rw, allow_ro, subscribe) = handle.split();
            S::allow_rw::<C, DRIVER_NUM, { rw_allow::READ }>(allow_rw, r_buf)?;
            S::allow_ro::<C, DRIVER_NUM, { ro_allow::WRITE }>(allow_ro, w_buf)?;
            S::subscribe::<_, _, C, DRIVER_NUM, { subscribe::COMPLETE }>(subscribe, &called)?;

            S::command(DRIVER_NUM, spi_controller_cmd::READ_WRITE_BYTES, len, 0)
                .to_result::<(), ErrorCode>()?;

            loop {
                S::yield_wait();
                if let Some((r0, status, _)) = called.get() {
                    assert_eq!(r0, len);
                    return match status {
                        0 => Ok(()),
                        e_status => Err(e_status.try_into().unwrap_or(ErrorCode::Fail)),
                    };
                }
            }
        })
    }

    pub fn spi_controller_write_sync(w_buf: &[u8], len: u32) -> Result<(), ErrorCode> {
        if len as usize > w_buf.len() {
            return Err(ErrorCode::NoMem);
        }

        let called: Cell<Option<(u32, u32, u32)>> = Cell::new(None);
        share::scope::<
            (
                AllowRo<_, DRIVER_NUM, { ro_allow::WRITE }>,
                Subscribe<_, DRIVER_NUM, { subscribe::COMPLETE }>,
            ),
            _,
            _,
        >(|handle| {
            let (allow_ro, subscribe) = handle.split();
            S::allow_ro::<C, DRIVER_NUM, { ro_allow::WRITE }>(allow_ro, w_buf)?;
            S::subscribe::<_, _, C, DRIVER_NUM, { subscribe::COMPLETE }>(subscribe, &called)?;

            S::command(DRIVER_NUM, spi_controller_cmd::READ_WRITE_BYTES, len, 0)
                .to_result::<(), ErrorCode>()?;

            loop {
                S::yield_wait();
                if let Some((r0, status, _)) = called.get() {
                    assert_eq!(r0, len);
                    return match status {
                        0 => Ok(()),
                        e_status => Err(e_status.try_into().unwrap_or(ErrorCode::Fail)),
                    };
                }
            }
        })
    }

    pub fn spi_controller_read_sync(r_buf: &mut [u8], len: u32) -> Result<(), ErrorCode> {
        if len as usize > r_buf.len() {
            return Err(ErrorCode::NoMem);
        }

        let called: Cell<Option<(u32, u32, u32)>> = Cell::new(None);
        share::scope::<
            (
                AllowRw<_, DRIVER_NUM, { rw_allow::READ }>,
                Subscribe<_, DRIVER_NUM, { subscribe::COMPLETE }>,
            ),
            _,
            _,
        >(|handle| {
            let (allow_rw, subscribe) = handle.split();
            S::allow_rw::<C, DRIVER_NUM, { rw_allow::READ }>(allow_rw, r_buf)?;
            S::subscribe::<_, _, C, DRIVER_NUM, { subscribe::COMPLETE }>(subscribe, &called)?;

            S::command(DRIVER_NUM, spi_controller_cmd::READ_BYTES, len, 0)
                .to_result::<(), ErrorCode>()?;

            loop {
                S::yield_wait();
                if let Some((r0, status, _)) = called.get() {
                    assert_eq!(r0, len);
                    return match status {
                        0 => Ok(()),
                        e_status => Err(e_status.try_into().unwrap_or(ErrorCode::Fail)),
                    };
                }
            }
        })
    }

    pub fn spi_controller_inplace_write_read_sync(
        r_buf: &mut [u8],
        len: u32,
    ) -> Result<(), ErrorCode> {
        if len as usize > r_buf.len() {
            return Err(ErrorCode::NoMem);
        }

        let called: Cell<Option<(u32, u32, u32)>> = Cell::new(None);
        share::scope::<
            (
                AllowRw<_, DRIVER_NUM, { rw_allow::READ }>,
                Subscribe<_, DRIVER_NUM, { subscribe::COMPLETE }>,
            ),
            _,
            _,
        >(|handle| {
            let (allow_rw, subscribe) = handle.split();
            S::allow_rw::<C, DRIVER_NUM, { rw_allow::READ }>(allow_rw, r_buf)?;
            S::subscribe::<_, _, C, DRIVER_NUM, { subscribe::COMPLETE }>(subscribe, &called)?;

            S::command(
                DRIVER_NUM,
                spi_controller_cmd::INPLACE_READ_WRITE_BYTES,
                len,
                0,
            )
            .to_result::<(), ErrorCode>()?;

            loop {
                S::yield_wait();
                if let Some((r0, status, _)) = called.get() {
                    assert_eq!(r0, len);
                    return match status {
                        0 => Ok(()),
                        e_status => Err(e_status.try_into().unwrap_or(ErrorCode::Fail)),
                    };
                }
            }
        })
    }

    // -------------------------------------------------------------------------
    // Bus configuration
    // -------------------------------------------------------------------------

    /// Asks for a clock rate in Hz.
    ///
    /// The kernel sets the closest rate its divider can produce, which is
    /// usually not the one asked for. Read it back with
    /// [`SpiController::get_baud_rate`] rather than assuming; a peripheral with
    /// a maximum will not thank you for the difference.
    ///
    /// Worth having as a runtime call rather than a board constant: some
    /// devices read far slower than they write — an ILI9341 display accepts
    /// writes past 10 MHz and register reads only to about 6.6 — so the rate
    /// that works is a property of the operation, not of the bus.
    pub fn set_baud_rate(rate: u32) -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, spi_controller_cmd::SET_BAUD, rate, 0).to_result()
    }

    /// The clock rate in Hz the kernel actually set.
    pub fn get_baud_rate() -> Result<u32, ErrorCode> {
        S::command(DRIVER_NUM, spi_controller_cmd::GET_BAUD, 0, 0).to_result()
    }

    /// Sets which clock edge samples the data.
    pub fn set_phase(phase: ClockPhase) -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, spi_controller_cmd::SET_PHASE, phase as u32, 0).to_result()
    }

    /// The clock phase currently set.
    pub fn get_phase() -> Result<ClockPhase, ErrorCode> {
        let phase: u32 = S::command(DRIVER_NUM, spi_controller_cmd::GET_PHASE, 0, 0).to_result()?;
        Ok(phase.into())
    }

    /// Sets the level the clock idles at.
    pub fn set_polarity(polarity: ClockPolarity) -> Result<(), ErrorCode> {
        S::command(
            DRIVER_NUM,
            spi_controller_cmd::SET_POLARITY,
            polarity as u32,
            0,
        )
        .to_result()
    }

    /// The clock polarity currently set.
    pub fn get_polarity() -> Result<ClockPolarity, ErrorCode> {
        let polarity: u32 =
            S::command(DRIVER_NUM, spi_controller_cmd::GET_POLARITY, 0, 0).to_result()?;
        Ok(polarity.into())
    }
}

/// Which clock edge samples the data. Half of what is usually called the SPI
/// mode, the other half being [`ClockPolarity`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClockPhase {
    SampleLeading = 0,
    SampleTrailing = 1,
}

/// The level the clock idles at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClockPolarity {
    IdleLow = 0,
    IdleHigh = 1,
}

impl From<u32> for ClockPhase {
    /// Anything but zero is trailing, which is how the capsule reads the
    /// argument on the way in — see command 7 in
    /// `capsules/core/src/spi_controller.rs`. Matching it here keeps a value
    /// written and read back the same value.
    fn from(value: u32) -> ClockPhase {
        match value {
            0 => ClockPhase::SampleLeading,
            _ => ClockPhase::SampleTrailing,
        }
    }
}

impl From<u32> for ClockPolarity {
    /// See [`ClockPhase::from`]; command 9 reads its argument the same way.
    fn from(value: u32) -> ClockPolarity {
        match value {
            0 => ClockPolarity::IdleLow,
            _ => ClockPolarity::IdleHigh,
        }
    }
}

#[cfg(test)]
mod tests;

/// System call configuration trait for `SpiController`.
pub trait Config:
    platform::allow_ro::Config + platform::allow_rw::Config + platform::subscribe::Config
{
}
impl<T: platform::allow_ro::Config + platform::allow_rw::Config + platform::subscribe::Config>
    Config for T
{
}

// -----------------------------------------------------------------------------
// Driver number and command IDs
// -----------------------------------------------------------------------------
const DRIVER_NUM: u32 = 0x20001;

#[allow(unused)]
mod subscribe {
    pub const COMPLETE: u32 = 0;
}

#[allow(unused)]
mod ro_allow {
    pub const WRITE: u32 = 0;
}

#[allow(unused)]
mod rw_allow {
    pub const READ: u32 = 0;
}

#[allow(unused)]
mod spi_controller_cmd {
    pub const EXISTS: u32 = 0;
    pub const READ_WRITE_BYTES: u32 = 2;
    pub const SET_BAUD: u32 = 5;
    pub const GET_BAUD: u32 = 6;
    pub const SET_PHASE: u32 = 7;
    pub const GET_PHASE: u32 = 8;
    pub const SET_POLARITY: u32 = 9;
    pub const GET_POLARITY: u32 = 10;
    pub const READ_BYTES: u32 = 11;
    pub const INPLACE_READ_WRITE_BYTES: u32 = 12;
}
