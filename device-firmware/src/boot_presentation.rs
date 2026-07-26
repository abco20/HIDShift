const BOOT_MAGIC: u32 = 0x4853_4250;
const BOOT_CHECK_XOR: u32 = 0x91d4_a36b;

#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut BOOT_REQUEST: [u32; 3] = [0; 3];

/// Returns and consumes the one-shot Mirror profile requested before a
/// software reset. Invalid or interrupted writes safely select Fallback.
pub fn take_mirror_profile() -> Option<u32> {
    // SAFETY: main is the sole owner and calls this before starting tasks or
    // interrupts. Volatile accesses avoid references to persistent static mut.
    unsafe {
        let request = &raw mut BOOT_REQUEST;
        let magic = core::ptr::read_volatile(core::ptr::addr_of!((*request)[0]));
        let profile_hash = core::ptr::read_volatile(core::ptr::addr_of!((*request)[1]));
        let check = core::ptr::read_volatile(core::ptr::addr_of!((*request)[2]));
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*request)[0]), 0);
        (magic == BOOT_MAGIC && check == profile_hash ^ BOOT_MAGIC ^ BOOT_CHECK_XOR)
            .then_some(profile_hash)
    }
}

pub fn request_mirror(profile_hash: u32) {
    // SAFETY: the blocking Device main loop is the sole writer. The checksum
    // makes a reset during these writes resolve to Fallback on the next boot.
    unsafe {
        let request = &raw mut BOOT_REQUEST;
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*request)[0]), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*request)[1]), profile_hash);
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!((*request)[2]),
            profile_hash ^ BOOT_MAGIC ^ BOOT_CHECK_XOR,
        );
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*request)[0]), BOOT_MAGIC);
    }
}
