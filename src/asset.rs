use crate::definitions::{api_v2::AssetId, PalletIndex};
use std::marker::PhantomData;

#[derive(Clone, Debug)]
pub enum Asset {
    Id(AssetId),
    MultiLocation(PalletIndex, AssetId),
}

impl Asset {
    const ID: &'static str = "Id";
    const MULTI_LOCATION: &'static str = "MultiLocation";
}

/*
pub struct AssetVisitor<R>(PhantomData<R>);

fn try_into_asset_id(
    number: impl TryInto<AssetId> + ToString + Copy,
) -> Result<AssetId, scale_decode::Error> {
    number.try_into().map_err(|_| {
        scale_decode::Error::new(ErrorKind::NumberOutOfRange {
            value: number.to_string(),
        })
    })
}

macro_rules! visit_number {
    ($visit:ident, $number:ty) => {
        fn $visit<'scale, 'resolver>(
            self,
            number: $number,
            type_id: &TypeIdFor<Self>,
        ) -> Result<Self::Value<'scale, 'resolver>, Self::Error> {
            self.visit_u32(try_into_asset_id(number)?, type_id)
        }
    };
}

macro_rules! visit_composite_or_tuple {
    ($visit:ident, $composite_or_tuple:ident) => {
        fn $visit<'scale, 'resolver>(
            self,
            composite_or_tuple: &mut $composite_or_tuple<'scale, 'resolver, Self::TypeResolver>,
            type_id: &TypeIdFor<Self>,
        ) -> Result<Self::Value<'scale, 'resolver>, Self::Error> {
            if composite_or_tuple.remaining() == 1 {
                // Shouldn't panic. We've just checked remaining items above.
                return composite_or_tuple
                    .decode_item(self)
                    .unwrap()
                    .map_err(|error| error.at_variant(Self::Value::ID));
            }

            let (pallet_index, asset_id) = (|| {
                let MultiLocation {
                    interior: Junctions::X2(first, second),
                    ..
                } = MultiLocation::into_visitor().$visit(composite_or_tuple, type_id)?;

                let Junction::PalletInstance(pallet_index) = first else {
                    return Err(scale_decode::Error::new(ErrorKind::CannotFindVariant {
                        got: first.name().into(),
                        expected: vec![Junction::PALLET_INSTANCE],
                    })
                    .at_idx(0));
                };

                let asset_id = (|| {
                    let Junction::GeneralIndex(general_index) = second else {
                        return Err(scale_decode::Error::new(ErrorKind::CannotFindVariant {
                            got: first.name().into(),
                            expected: vec![Junction::GENERAL_INDEX],
                        }));
                    };

                    let asset_id = general_index.try_into().map_err(|_| {
                        scale_decode::Error::new(ErrorKind::NumberOutOfRange {
                            value: general_index.to_string(),
                        })
                    })?;

                    Ok(asset_id)
                })()
                .map_err(|error| error.at_idx(1))?;

                Ok((pallet_index, asset_id))
            })()
            .map_err(|error| error.at_variant(Self::Value::MULTI_LOCATION))?;

            Ok(Self::Value::MultiLocation(pallet_index, asset_id))
        }
    };
}

impl<R: TypeResolver> Visitor for AssetVisitor<R> {
    type Value<'scale, 'resolver> = Asset;
    type Error = scale_decode::Error;
    type TypeResolver = R;

    fn visit_u32<'scale, 'resolver>(
        self,
        value: u32,
        _: &TypeIdFor<Self>,
    ) -> Result<Self::Value<'scale, 'resolver>, Self::Error> {
        Ok(Self::Value::Id(value))
    }

    fn visit_u8<'scale, 'resolver>(
        self,
        value: u8,
        type_id: &TypeIdFor<Self>,
    ) -> Result<Self::Value<'scale, 'resolver>, Self::Error> {
        self.visit_u32(value.into(), type_id)
    }

    fn visit_u16<'scale, 'resolver>(
        self,
        value: u16,
        type_id: &TypeIdFor<Self>,
    ) -> Result<Self::Value<'scale, 'resolver>, Self::Error> {
        self.visit_u32(value.into(), type_id)
    }

    visit_number!(visit_i8, i8);
    visit_number!(visit_i16, i16);
    visit_number!(visit_i32, i32);
    visit_number!(visit_u64, u64);
    visit_number!(visit_i64, i64);
    visit_number!(visit_i128, i128);
    visit_number!(visit_u128, u128);

    visit_composite_or_tuple!(visit_tuple, Tuple);
    visit_composite_or_tuple!(visit_composite, Composite);
}

impl IntoVisitor for Asset {
    type AnyVisitor<R: TypeResolver> = AssetVisitor<R>;

    fn into_visitor<R: TypeResolver>() -> Self::AnyVisitor<R> {
        AssetVisitor(PhantomData)
    }
}

impl EncodeAsType for Asset {
    fn encode_as_type_to<R: TypeResolver>(
        &self,
        type_id: &R::TypeId,
        types: &R,
        out: &mut Vec<u8>,
    ) -> Result<(), scale_encode::Error> {
        match self {
            Self::Id(id) => id.encode_as_type_to(type_id, types, out),
            Self::MultiLocation(assets_pallet, asset_id) => {
                MultiLocation::new(*assets_pallet, *asset_id).encode_as_type_to(type_id, types, out)
            }
        }
    }
}

impl Encode for Asset {
    fn size_hint(&self) -> usize {
        match self {
            Self::Id(id) => id.size_hint(),
            Self::MultiLocation(assets_pallet, asset_id) => {
                MultiLocation::new(*assets_pallet, *asset_id).size_hint()
            }
        }
    }

    fn encode_to<T: Output + ?Sized>(&self, dest: &mut T) {
        match self {
            Self::Id(id) => id.encode_to(dest),
            Self::MultiLocation(assets_pallet, asset_id) => {
                MultiLocation::new(*assets_pallet, *asset_id).encode_to(dest);
            }
        }
    }
}

#[derive(EncodeAsType, DecodeAsType, Encode)]
struct MultiLocation {
    parents: u8,
    interior: Junctions,
}

impl MultiLocation {
    fn new(assets_pallet: PalletIndex, asset_id: AssetId) -> Self {
        Self {
            parents: 0,
            interior: Junctions::X2(
                Junction::PalletInstance(assets_pallet),
                Junction::GeneralIndex(asset_id.into()),
            ),
        }
    }
}

#[derive(EncodeAsType, DecodeAsType, Encode)]
enum Junctions {
    #[codec(index = 2)]
    X2(Junction, Junction),
}

#[derive(EncodeAsType, DecodeAsType, Encode)]
enum Junction {
    #[codec(index = 4)]
    PalletInstance(PalletIndex),
    #[codec(index = 5)]
    GeneralIndex(u128),
}

impl Junction {
    const PALLET_INSTANCE: &'static str = "PalletInstance";
    const GENERAL_INDEX: &'static str = "GeneralIndex";

    fn name(&self) -> &str {
        match self {
            Self::PalletInstance(_) => Self::PALLET_INSTANCE,
            Self::GeneralIndex(_) => Self::GENERAL_INDEX,
        }
    }
}
*/
