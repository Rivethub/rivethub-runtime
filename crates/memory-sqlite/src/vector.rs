pub(crate) fn normalize(vector: &mut [f32]) -> bool {
    let norm = vector
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return false;
    }
    for value in vector {
        #[expect(clippy::cast_possible_truncation)]
        let normalized = (f64::from(*value) / norm) as f32;
        *value = normalized;
    }
    true
}

pub(crate) fn pack(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

pub(crate) fn unpack(bytes: &[u8]) -> Result<Vec<f32>, ()> {
    if !bytes.len().is_multiple_of(4) {
        return Err(());
    }
    let mut values = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.as_chunks::<4>().0 {
        let value = f32::from_le_bytes(*chunk);
        if !value.is_finite() {
            return Err(());
        }
        values.push(value);
    }
    Ok(values)
}
