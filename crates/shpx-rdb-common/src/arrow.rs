//! Arrow primitive 配列からの値取り出し。複数 driver の `util.rs` で 1 行ずつ
//! 重複していたので集約する。

use arrow_array::{Array, ArrowPrimitiveType, PrimitiveArray};

/// Arrow primitive 配列の指定行から native 値を取り出す。
///
/// downcast 失敗は schema mismatch でプログラムバグなので panic する（呼び出し側で
/// `DataType` を確認済みである前提）。`Send` 等の制約は不要なため `&dyn Array` で受ける。
pub fn primitive<T: ArrowPrimitiveType>(array: &dyn Array, row: usize) -> T::Native {
    array
        .as_any()
        .downcast_ref::<PrimitiveArray<T>>()
        .expect("primitive downcast")
        .value(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{types::Int32Type, Int32Array};

    #[test]
    fn primitive_reads_value() {
        let arr = Int32Array::from(vec![10, 20, 30]);
        assert_eq!(primitive::<Int32Type>(&arr, 0), 10);
        assert_eq!(primitive::<Int32Type>(&arr, 2), 30);
    }

    #[test]
    #[should_panic(expected = "primitive downcast")]
    fn primitive_panics_on_type_mismatch() {
        use arrow_array::types::Int64Type;
        let arr = Int32Array::from(vec![10]);
        // i32 配列を i64 として読もうとして downcast 失敗 → panic。
        let _: i64 = primitive::<Int64Type>(&arr, 0);
    }
}
