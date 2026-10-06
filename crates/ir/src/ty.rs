use mollie_const::ConstantValue;

use crate::MollieType;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Field {
    pub ty: MollieType,
    pub offset: i32,
    pub default_value: Option<ConstantValue>,
}

/// Memory layout of a structure: every field is placed at the next offset
/// that's a multiple of its alignment.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct Struct {
    pub fields: Vec<Field>,
    pub size: u32,
    pub align: u32,
}

impl Struct {
    /// Lays out fields after `start` bytes (e.g. after a header).
    pub fn with_offset<T: IntoIterator<Item = (MollieType, Option<ConstantValue>)>>(start: u32, fields_iter: T) -> Self {
        let mut fields = Vec::new();
        let mut offset = start;
        let mut align = 1;

        for (ty, default_value) in fields_iter {
            let field_align = ty.align().max(1);

            offset = offset.next_multiple_of(field_align);
            align = align.max(field_align);

            fields.push(Field {
                ty,
                offset: offset.cast_signed(),
                default_value,
            });

            offset += ty.bytes();
        }

        Self {
            fields,
            size: offset.next_multiple_of(align),
            align,
        }
    }

    pub fn new<T: IntoIterator<Item = (MollieType, Option<ConstantValue>)>>(fields_iter: T) -> Self {
        Self::with_offset(0, fields_iter)
    }
}
