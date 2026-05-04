/// Specification IDs and their activation points.
///
/// Information was obtained from the [Ethereum Execution Specifications](https://github.com/ethereum/execution-specs).
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
#[allow(non_camel_case_types)]
pub enum SpecId {
    /// Frontier
    ///
    /// Activated at block 1
    FRONTIER = 0,
    /// Homestead
    ///
    /// Activated at block 1150000
    HOMESTEAD,
    /// Tangerine Whistle
    ///
    /// Activated at block 2463000
    TANGERINE,
    /// Spurious Dragon
    ///
    /// Activated at block 2675000
    SPURIOUS_DRAGON,
    /// Byzantium
    ///
    /// Activated at block 4370000
    BYZANTIUM,
    /// Petersburg
    ///
    /// Activated at block 7280000
    PETERSBURG,
    /// Istanbul
    ///
    /// Activated at block 9069000
    ISTANBUL,
    /// Berlin
    ///
    /// Activated at block 12244000
    BERLIN,
    /// London
    ///
    /// Activated at block 12965000
    LONDON,
    /// Paris/Merge
    ///
    /// Activated at block 15537394
    MERGE,
    /// Shanghai
    ///
    /// Activated at block 17034870 (timestamp 1681338455)
    SHANGHAI,
    /// Cancun
    ///
    /// Activated at block 19426587 (timestamp 1710338135)
    CANCUN,
    /// Prague
    ///
    /// Activated at block 22431084
    PRAGUE,
    /// Osaka
    ///
    /// Activated at block 23935694
    #[default]
    OSAKA,
    /// Amsterdam
    ///
    /// Activated at block TBD
    AMSTERDAM,
}

impl SpecId {
    /// Default specification ID.
    pub const DEFAULT: Self = Self::OSAKA;

    /// Latest known specification ID.
    #[doc(alias = "MAX")]
    pub const NEXT: Self = Self::AMSTERDAM;

    /// Number of SpecId variants.
    pub const COUNT: usize = Self::NEXT as usize + 1;

    /// Returns the specification ID for a raw byte.
    #[inline]
    pub const fn try_from_u8(spec_id: u8) -> Option<Self> {
        if spec_id <= Self::NEXT as u8 {
            // SAFETY: `spec_id` is within the valid variant range.
            return Some(unsafe { core::mem::transmute::<u8, Self>(spec_id) });
        }
        None
    }

    /// Returns `true` if this specification enables `other`.
    #[inline]
    pub const fn enables(self, other: Self) -> bool {
        self as u8 >= other as u8
    }

    /// Returns `true` if `other` is enabled in this specification.
    #[deprecated(note = "use SpecId::enables instead")]
    #[inline]
    pub const fn is_enabled_in(self, other: Self) -> bool {
        self.enables(other)
    }
}

impl From<SpecId> for u8 {
    #[inline]
    fn from(spec_id: SpecId) -> Self {
        spec_id as Self
    }
}

impl TryFrom<u8> for SpecId {
    type Error = u8;

    #[inline]
    fn try_from(value: u8) -> core::result::Result<Self, Self::Error> {
        Self::try_from_u8(value).ok_or(value)
    }
}

/// Maps a runtime specification ID to a compile-time EVM config named `SPEC`.
#[macro_export]
macro_rules! spec_to_generic {
    (@spec $spec_id:ident, $e:expr) => {{
        const SPEC_ID: u8 = $crate::SpecId::$spec_id as u8;
        #[allow(clippy::upper_case_acronyms)]
        type SPEC = $crate::BaseEvmConfig<SPEC_ID>;
        $e
    }};
    ($spec_id:expr, $e:expr) => {{
        match $spec_id {
            $crate::SpecId::FRONTIER => {
                $crate::spec_to_generic!(@spec FRONTIER, $e)
            }
            $crate::SpecId::HOMESTEAD => {
                $crate::spec_to_generic!(@spec HOMESTEAD, $e)
            }
            $crate::SpecId::TANGERINE => {
                $crate::spec_to_generic!(@spec TANGERINE, $e)
            }
            $crate::SpecId::SPURIOUS_DRAGON => {
                $crate::spec_to_generic!(@spec SPURIOUS_DRAGON, $e)
            }
            $crate::SpecId::BYZANTIUM => {
                $crate::spec_to_generic!(@spec BYZANTIUM, $e)
            }
            $crate::SpecId::PETERSBURG => {
                $crate::spec_to_generic!(@spec PETERSBURG, $e)
            }
            $crate::SpecId::ISTANBUL => {
                $crate::spec_to_generic!(@spec ISTANBUL, $e)
            }
            $crate::SpecId::BERLIN => {
                $crate::spec_to_generic!(@spec BERLIN, $e)
            }
            $crate::SpecId::LONDON => {
                $crate::spec_to_generic!(@spec LONDON, $e)
            }
            $crate::SpecId::MERGE => {
                $crate::spec_to_generic!(@spec MERGE, $e)
            }
            $crate::SpecId::SHANGHAI => {
                $crate::spec_to_generic!(@spec SHANGHAI, $e)
            }
            $crate::SpecId::CANCUN => {
                $crate::spec_to_generic!(@spec CANCUN, $e)
            }
            $crate::SpecId::PRAGUE => {
                $crate::spec_to_generic!(@spec PRAGUE, $e)
            }
            $crate::SpecId::OSAKA => {
                $crate::spec_to_generic!(@spec OSAKA, $e)
            }
            $crate::SpecId::AMSTERDAM => {
                $crate::spec_to_generic!(@spec AMSTERDAM, $e)
            }
        }
    }};
}
