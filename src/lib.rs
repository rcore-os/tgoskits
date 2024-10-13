#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

use core::hash::{Hash, Hasher};
use core::ops::*;

use bitmaps::{BitOps, Bitmap, Bits, BitsImpl};

/// A compact array of bits which represents a set of physical CPUs,
/// implemented based on [bitmaps::Bitmap](https://docs.rs/bitmaps/latest/bitmaps/struct.Bitmap.html).
///
/// The type used to store the cpumask will be the minimum unsigned integer type
/// required to fit the number of bits, from `u8` to `u128`. If the size is 1,
/// `bool` is used. If the size exceeds 128, an array of `u128` will be used,
/// sized as appropriately. The maximum supported size is currently 1024,
/// represented by an array `[u128; 8]`.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub struct CpuMask<const SIZE: usize>
where
    BitsImpl<{ SIZE }>: Bits,
{
    value: Bitmap<{ SIZE }>,
}

impl<const SIZE: usize> core::fmt::Debug for CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "cpumask: [")?;
        for cpu in self.into_iter() {
            write!(f, "{}, ", cpu)?;
        }
        write!(f, "]")
    }
}

impl<const SIZE: usize> Hash for CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
    <BitsImpl<{ SIZE }> as Bits>::Store: Hash,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.as_value().hash(state)
    }
}

impl<const SIZE: usize> PartialOrd for CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
    <BitsImpl<{ SIZE }> as Bits>::Store: PartialOrd,
{
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.value.as_value().partial_cmp(other.value.as_value())
    }
}

impl<const SIZE: usize> Ord for CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
    <BitsImpl<{ SIZE }> as Bits>::Store: Ord,
{
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.value.as_value().cmp(other.value.as_value())
    }
}

impl<const SIZE: usize> CpuMask<{ SIZE }>
where
    BitsImpl<SIZE>: Bits,
{
    /// Construct a cpumask with every bit set to `false`.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a cpumask with every bit set to `true`.
    #[inline]
    pub fn full() -> Self {
        Self {
            value: Bitmap::mask(SIZE),
        }
    }

    /// Construct a cpumask where every bit with index less than `bits` is
    /// `true`, and every other bit is `false`.
    #[inline]
    pub fn mask(bits: usize) -> Self {
        debug_assert!(bits <= SIZE);
        Self {
            value: Bitmap::mask(bits),
        }
    }

    /// Construct a cpumask from a value of the same type as its backing store.
    #[inline]
    pub fn from_value(data: <BitsImpl<SIZE> as Bits>::Store) -> Self {
        Self {
            value: Bitmap::from_value(data),
        }
    }

    /// Construct a cpumask from a raw `usize` value.
    /// The value must be less than `2^SIZE`, panick if the value is too large.
    pub fn from_raw_bits(value: usize) -> Self {
        assert!(value >> SIZE == 0);

        let mut bit_map = Bitmap::new();
        let mut i = 0;
        while i < SIZE {
            if value & (1 << i) != 0 {
                bit_map.set(i, true);
            }
            i += 1;
        }

        Self { value: bit_map }
    }

    /// Construct a cpumask with a single bit set at the specified index.
    /// The value must be less than `SIZE`, panick if the value is too large.
    pub fn one_shot(index: usize) -> Self {
        assert!(index < SIZE);
        let mut bit_map = Bitmap::new();
        bit_map.set(index, true);
        Self { value: bit_map }
    }

    /// Convert this cpumask into a value of the type of its backing store.
    #[inline]
    pub fn into_value(self) -> <BitsImpl<SIZE> as Bits>::Store {
        self.value.into_value()
    }

    /// Get a reference to this cpumask's backing store.
    #[inline]
    pub fn as_value(&self) -> &<BitsImpl<SIZE> as Bits>::Store {
        self.value.as_value()
    }

    /// Get this cpumask as a slice of bytes.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        self.value.as_bytes()
    }

    /// Count the number of `true` bits in the cpumask.
    #[inline]
    pub fn len(self) -> usize {
        self.value.len()
    }

    /// Test if the cpumask contains only `false` bits.
    #[inline]
    pub fn is_empty(self) -> bool {
        self.first_index().is_none()
    }

    /// Test if the cpumask contains only `true` bits.
    #[inline]
    pub fn is_full(self) -> bool {
        self.first_false_index().is_none()
    }

    /// Get the value of the bit at a given index.
    #[inline]
    pub fn get(self, index: usize) -> bool {
        debug_assert!(index < SIZE);
        <BitsImpl<SIZE> as Bits>::Store::get(&self.into_value(), index)
    }

    /// Set the value of the bit at a given index.
    ///
    /// Returns the previous value of the bit.
    #[inline]
    pub fn set(&mut self, index: usize, value: bool) -> bool {
        debug_assert!(index < SIZE);
        self.value.set(index, value)
    }

    /// Find the index of the first `true` bit in the cpumask.
    #[inline]
    pub fn first_index(self) -> Option<usize> {
        <BitsImpl<SIZE> as Bits>::Store::first_index(&self.into_value())
    }

    /// Find the index of the last `true` bit in the cpumask.
    #[inline]
    pub fn last_index(self) -> Option<usize> {
        <BitsImpl<SIZE> as Bits>::Store::last_index(&self.into_value())
    }

    /// Find the index of the first `true` bit in the cpumask after `index`.
    #[inline]
    pub fn next_index(self, index: usize) -> Option<usize> {
        <BitsImpl<SIZE> as Bits>::Store::next_index(&self.into_value(), index)
    }

    /// Find the index of the last `true` bit in the cpumask before `index`.
    #[inline]
    pub fn prev_index(self, index: usize) -> Option<usize> {
        <BitsImpl<SIZE> as Bits>::Store::prev_index(&self.into_value(), index)
    }

    /// Find the index of the first `false` bit in the cpumask.
    #[inline]
    pub fn first_false_index(self) -> Option<usize> {
        <BitsImpl<SIZE> as Bits>::corrected_first_false_index(&self.into_value())
    }

    /// Find the index of the last `false` bit in the cpumask.
    #[inline]
    pub fn last_false_index(self) -> Option<usize> {
        <BitsImpl<SIZE> as Bits>::corrected_last_false_index(&self.into_value())
    }

    /// Find the index of the first `false` bit in the cpumask after `index`.
    #[inline]
    pub fn next_false_index(self, index: usize) -> Option<usize> {
        <BitsImpl<SIZE> as Bits>::corrected_next_false_index(&self.into_value(), index)
    }

    /// Find the index of the first `false` bit in the cpumask before `index`.
    #[inline]
    pub fn prev_false_index(self, index: usize) -> Option<usize> {
        <BitsImpl<SIZE> as Bits>::Store::prev_false_index(&self.into_value(), index)
    }

    /// Invert all the bits in the cpumask.
    #[inline]
    pub fn invert(&mut self) {
        self.value.invert();
    }
}

impl<'a, const SIZE: usize> IntoIterator for &'a CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
{
    type Item = usize;
    type IntoIter = Iter<'a, { SIZE }>;

    fn into_iter(self) -> Self::IntoIter {
        Iter {
            head: None,
            tail: Some(SIZE + 1),
            data: self,
        }
    }
}

impl<const SIZE: usize> BitAnd for CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
{
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self::Output {
        Self {
            value: self.value.bitand(rhs.value),
        }
    }
}

impl<const SIZE: usize> BitOr for CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
{
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self {
            value: self.value.bitor(rhs.value),
        }
    }
}

impl<const SIZE: usize> BitXor for CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
{
    type Output = Self;
    fn bitxor(self, rhs: Self) -> Self::Output {
        Self {
            value: self.value.bitxor(rhs.value),
        }
    }
}

impl<const SIZE: usize> Not for CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
{
    type Output = Self;
    fn not(self) -> Self::Output {
        Self {
            value: self.value.not(),
        }
    }
}

impl<const SIZE: usize> BitAndAssign for CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
{
    fn bitand_assign(&mut self, rhs: Self) {
        self.value.bitand_assign(rhs.value)
    }
}

impl<const SIZE: usize> BitOrAssign for CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
{
    fn bitor_assign(&mut self, rhs: Self) {
        self.value.bitor_assign(rhs.value)
    }
}

impl<const SIZE: usize> BitXorAssign for CpuMask<{ SIZE }>
where
    BitsImpl<{ SIZE }>: Bits,
{
    fn bitxor_assign(&mut self, rhs: Self) {
        self.value.bitxor_assign(rhs.value)
    }
}

impl From<[u128; 2]> for CpuMask<256> {
    fn from(data: [u128; 2]) -> Self {
        CpuMask { value: data.into() }
    }
}

impl From<[u128; 3]> for CpuMask<384> {
    fn from(data: [u128; 3]) -> Self {
        CpuMask { value: data.into() }
    }
}

impl From<[u128; 4]> for CpuMask<512> {
    fn from(data: [u128; 4]) -> Self {
        CpuMask { value: data.into() }
    }
}

impl From<[u128; 5]> for CpuMask<640> {
    fn from(data: [u128; 5]) -> Self {
        CpuMask { value: data.into() }
    }
}

impl From<[u128; 6]> for CpuMask<768> {
    fn from(data: [u128; 6]) -> Self {
        CpuMask { value: data.into() }
    }
}

impl From<[u128; 7]> for CpuMask<896> {
    fn from(data: [u128; 7]) -> Self {
        CpuMask { value: data.into() }
    }
}

impl From<[u128; 8]> for CpuMask<1024> {
    fn from(data: [u128; 8]) -> Self {
        CpuMask { value: data.into() }
    }
}

impl From<CpuMask<256>> for [u128; 2] {
    fn from(cpumask: CpuMask<256>) -> Self {
        cpumask.into_value()
    }
}

impl From<CpuMask<384>> for [u128; 3] {
    fn from(cpumask: CpuMask<384>) -> Self {
        cpumask.into_value()
    }
}

impl From<CpuMask<512>> for [u128; 4] {
    fn from(cpumask: CpuMask<512>) -> Self {
        cpumask.into_value()
    }
}

impl From<CpuMask<640>> for [u128; 5] {
    fn from(cpumask: CpuMask<640>) -> Self {
        cpumask.into_value()
    }
}

impl From<CpuMask<768>> for [u128; 6] {
    fn from(cpumask: CpuMask<768>) -> Self {
        cpumask.into_value()
    }
}

impl From<CpuMask<896>> for [u128; 7] {
    fn from(cpumask: CpuMask<896>) -> Self {
        cpumask.into_value()
    }
}

impl From<CpuMask<1024>> for [u128; 8] {
    fn from(cpumask: CpuMask<1024>) -> Self {
        cpumask.into_value()
    }
}

/// An iterator over the indices in a cpumask which are `true`.
///
/// This yields a sequence of `usize` indices, not their contents (which are
/// always `true` anyway, by definition).
///
/// # Examples
///
/// ```rust
/// # use cpumask::CpuMask;
/// let mut cpumask: CpuMask<10> = CpuMask::new();
/// cpumask.set(3, true);
/// cpumask.set(5, true);
/// cpumask.set(8, true);
/// let true_indices: Vec<usize> = cpumask.into_iter().collect();
/// assert_eq!(vec![3, 5, 8], true_indices);
/// ```
#[derive(Clone, Debug)]
pub struct Iter<'a, const SIZE: usize>
where
    BitsImpl<SIZE>: Bits,
{
    head: Option<usize>,
    tail: Option<usize>,
    data: &'a CpuMask<{ SIZE }>,
}

impl<'a, const SIZE: usize> Iterator for Iter<'a, SIZE>
where
    BitsImpl<{ SIZE }>: Bits,
{
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        let result;

        match self.head {
            None => {
                result = self.data.first_index();
            }
            Some(index) => {
                if index >= SIZE {
                    result = None
                } else {
                    result = self.data.next_index(index);
                }
            }
        }

        if let Some(index) = result {
            if let Some(tail) = self.tail {
                if tail < index {
                    self.head = Some(SIZE + 1);
                    self.tail = None;
                    return None;
                }
            } else {
                // tail is already done
                self.head = Some(SIZE + 1);
                return None;
            }

            self.head = Some(index);
        } else {
            self.head = Some(SIZE + 1);
        }

        result
    }
}

impl<'a, const SIZE: usize> DoubleEndedIterator for Iter<'a, SIZE>
where
    BitsImpl<{ SIZE }>: Bits,
{
    fn next_back(&mut self) -> Option<Self::Item> {
        let result;

        match self.tail {
            None => {
                result = None;
            }
            Some(index) => {
                if index >= SIZE {
                    result = self.data.last_index();
                } else {
                    result = self.data.prev_index(index);
                }
            }
        }

        if let Some(index) = result {
            if let Some(head) = self.head {
                if head > index {
                    self.head = Some(SIZE + 1);
                    self.tail = None;
                    return None;
                }
            }

            self.tail = Some(index);
        } else {
            self.tail = None;
        }

        result
    }
}
