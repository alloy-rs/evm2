use alloy_primitives::{Address, Bytes, FixedBytes, U256};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::BTreeMap, marker::PhantomData, mem::MaybeUninit};
use wincode::{
    ReadResult, SchemaRead, SchemaWrite, WriteResult,
    config::Config,
    error::ReadError,
    io::{Reader, Writer},
};

pub(crate) struct AddressSchema;

unsafe impl<C: Config> SchemaWrite<C> for AddressSchema {
    type Src = Address;

    fn size_of(src: &Self::Src) -> WriteResult<usize> {
        let mut bytes = [0; 20];
        bytes.copy_from_slice(src.as_slice());
        <[u8; 20] as SchemaWrite<C>>::size_of(&bytes)
    }

    fn write(writer: impl Writer, src: &Self::Src) -> WriteResult<()> {
        let mut bytes = [0; 20];
        bytes.copy_from_slice(src.as_slice());
        <[u8; 20] as SchemaWrite<C>>::write(writer, &bytes)
    }
}

unsafe impl<'de, C: Config> SchemaRead<'de, C> for AddressSchema {
    type Dst = Address;

    fn read(reader: impl Reader<'de>, dst: &mut MaybeUninit<Self::Dst>) -> ReadResult<()> {
        let bytes = <[u8; 20] as SchemaRead<C>>::get(reader)?;
        dst.write(Address::from_slice(&bytes));
        Ok(())
    }
}

pub(crate) struct FixedBytesSchema<const N: usize>;

unsafe impl<const N: usize, C: Config> SchemaWrite<C> for FixedBytesSchema<N> {
    type Src = FixedBytes<N>;

    fn size_of(src: &Self::Src) -> WriteResult<usize> {
        <[u8; N] as SchemaWrite<C>>::size_of(&src.0)
    }

    fn write(writer: impl Writer, src: &Self::Src) -> WriteResult<()> {
        <[u8; N] as SchemaWrite<C>>::write(writer, &src.0)
    }
}

unsafe impl<'de, const N: usize, C: Config> SchemaRead<'de, C> for FixedBytesSchema<N> {
    type Dst = FixedBytes<N>;

    fn read(reader: impl Reader<'de>, dst: &mut MaybeUninit<Self::Dst>) -> ReadResult<()> {
        let bytes = <[u8; N] as SchemaRead<C>>::get(reader)?;
        dst.write(FixedBytes(bytes));
        Ok(())
    }
}

pub(crate) type B256Schema = FixedBytesSchema<32>;
pub(crate) type FixedBytes8Schema = FixedBytesSchema<8>;

pub(crate) struct BytesSchema;

unsafe impl<C: Config> SchemaWrite<C> for BytesSchema {
    type Src = Bytes;

    fn size_of(src: &Self::Src) -> WriteResult<usize> {
        <Vec<u8> as SchemaWrite<C>>::size_of(&src.to_vec())
    }

    fn write(writer: impl Writer, src: &Self::Src) -> WriteResult<()> {
        <Vec<u8> as SchemaWrite<C>>::write(writer, &src.to_vec())
    }
}

unsafe impl<'de, C: Config> SchemaRead<'de, C> for BytesSchema {
    type Dst = Bytes;

    fn read(reader: impl Reader<'de>, dst: &mut MaybeUninit<Self::Dst>) -> ReadResult<()> {
        let bytes = <Vec<u8> as SchemaRead<C>>::get(reader)?;
        dst.write(Bytes::from(bytes));
        Ok(())
    }
}

pub(crate) struct U256Schema;

unsafe impl<C: Config> SchemaWrite<C> for U256Schema {
    type Src = U256;

    fn size_of(src: &Self::Src) -> WriteResult<usize> {
        <[u8; 32] as SchemaWrite<C>>::size_of(&src.to_be_bytes::<32>())
    }

    fn write(writer: impl Writer, src: &Self::Src) -> WriteResult<()> {
        <[u8; 32] as SchemaWrite<C>>::write(writer, &src.to_be_bytes::<32>())
    }
}

unsafe impl<'de, C: Config> SchemaRead<'de, C> for U256Schema {
    type Dst = U256;

    fn read(reader: impl Reader<'de>, dst: &mut MaybeUninit<Self::Dst>) -> ReadResult<()> {
        let bytes = <[u8; 32] as SchemaRead<C>>::get(reader)?;
        dst.write(U256::from_be_bytes(bytes));
        Ok(())
    }
}

pub(crate) struct OptionSchema<S>(PhantomData<S>);

unsafe impl<C, S> SchemaWrite<C> for OptionSchema<S>
where
    C: Config,
    S: SchemaWrite<C>,
    S::Src: Sized,
{
    type Src = Option<S::Src>;

    fn size_of(src: &Self::Src) -> WriteResult<usize> {
        let discriminant = <bool as SchemaWrite<C>>::size_of(&src.is_some())?;
        let value = match src {
            Some(value) => S::size_of(value)?,
            None => 0,
        };
        Ok(discriminant + value)
    }

    fn write(mut writer: impl Writer, src: &Self::Src) -> WriteResult<()> {
        <bool as SchemaWrite<C>>::write(writer.by_ref(), &src.is_some())?;
        if let Some(value) = src {
            S::write(writer, value)?;
        }
        Ok(())
    }
}

unsafe impl<'de, C, S> SchemaRead<'de, C> for OptionSchema<S>
where
    C: Config,
    S: SchemaRead<'de, C>,
{
    type Dst = Option<S::Dst>;

    fn read(mut reader: impl Reader<'de>, dst: &mut MaybeUninit<Self::Dst>) -> ReadResult<()> {
        let is_some = <bool as SchemaRead<C>>::get(reader.by_ref())?;
        let value = if is_some { Some(S::get(reader)?) } else { None };
        dst.write(value);
        Ok(())
    }
}

pub(crate) struct VecSchema<S>(PhantomData<S>);

unsafe impl<C, S> SchemaWrite<C> for VecSchema<S>
where
    C: Config,
    S: SchemaWrite<C>,
    S::Src: Sized,
{
    type Src = Vec<S::Src>;

    fn size_of(src: &Self::Src) -> WriteResult<usize> {
        let mut size = <usize as SchemaWrite<C>>::size_of(&src.len())?;
        for value in src {
            size += S::size_of(value)?;
        }
        Ok(size)
    }

    fn write(mut writer: impl Writer, src: &Self::Src) -> WriteResult<()> {
        <usize as SchemaWrite<C>>::write(writer.by_ref(), &src.len())?;
        for value in src {
            S::write(writer.by_ref(), value)?;
        }
        Ok(())
    }
}

unsafe impl<'de, C, S> SchemaRead<'de, C> for VecSchema<S>
where
    C: Config,
    S: SchemaRead<'de, C>,
{
    type Dst = Vec<S::Dst>;

    fn read(mut reader: impl Reader<'de>, dst: &mut MaybeUninit<Self::Dst>) -> ReadResult<()> {
        let len = <usize as SchemaRead<C>>::get(reader.by_ref())?;
        let mut values = Vec::with_capacity(len);
        for _ in 0..len {
            values.push(S::get(reader.by_ref())?);
        }
        dst.write(values);
        Ok(())
    }
}

pub(crate) struct BTreeMapSchema<K, V>(PhantomData<(K, V)>);

unsafe impl<C, K, V> SchemaWrite<C> for BTreeMapSchema<K, V>
where
    C: Config,
    K: SchemaWrite<C>,
    K::Src: Ord + Sized,
    V: SchemaWrite<C>,
    V::Src: Sized,
{
    type Src = BTreeMap<K::Src, V::Src>;

    fn size_of(src: &Self::Src) -> WriteResult<usize> {
        let mut size = <usize as SchemaWrite<C>>::size_of(&src.len())?;
        for (key, value) in src {
            size += K::size_of(key)? + V::size_of(value)?;
        }
        Ok(size)
    }

    fn write(mut writer: impl Writer, src: &Self::Src) -> WriteResult<()> {
        <usize as SchemaWrite<C>>::write(writer.by_ref(), &src.len())?;
        for (key, value) in src {
            K::write(writer.by_ref(), key)?;
            V::write(writer.by_ref(), value)?;
        }
        Ok(())
    }
}

unsafe impl<'de, C, K, V> SchemaRead<'de, C> for BTreeMapSchema<K, V>
where
    C: Config,
    K: SchemaRead<'de, C>,
    K::Dst: Ord,
    V: SchemaRead<'de, C>,
{
    type Dst = BTreeMap<K::Dst, V::Dst>;

    fn read(mut reader: impl Reader<'de>, dst: &mut MaybeUninit<Self::Dst>) -> ReadResult<()> {
        let len = <usize as SchemaRead<C>>::get(reader.by_ref())?;
        let mut values = BTreeMap::new();
        for _ in 0..len {
            values.insert(K::get(reader.by_ref())?, V::get(reader.by_ref())?);
        }
        dst.write(values);
        Ok(())
    }
}

pub(crate) struct JsonSchema<T>(PhantomData<T>);

unsafe impl<C, T> SchemaWrite<C> for JsonSchema<T>
where
    C: Config,
    T: Serialize,
{
    type Src = T;

    fn size_of(src: &Self::Src) -> WriteResult<usize> {
        let bytes = serde_json::to_vec(src)
            .map_err(|_| wincode::error::WriteError::Custom("JSON encode failed"))?;
        <Vec<u8> as SchemaWrite<C>>::size_of(&bytes)
    }

    fn write(writer: impl Writer, src: &Self::Src) -> WriteResult<()> {
        let bytes = serde_json::to_vec(src)
            .map_err(|_| wincode::error::WriteError::Custom("JSON encode failed"))?;
        <Vec<u8> as SchemaWrite<C>>::write(writer, &bytes)
    }
}

unsafe impl<'de, C, T> SchemaRead<'de, C> for JsonSchema<T>
where
    C: Config,
    T: DeserializeOwned,
{
    type Dst = T;

    fn read(reader: impl Reader<'de>, dst: &mut MaybeUninit<Self::Dst>) -> ReadResult<()> {
        let bytes = <Vec<u8> as SchemaRead<C>>::get(reader)?;
        let value =
            serde_json::from_slice(&bytes).map_err(|_| ReadError::Custom("JSON decode failed"))?;
        dst.write(value);
        Ok(())
    }
}
