//! Small, deterministic perceptual hash implementation.
//!
//! pHash is deliberately only a coarse candidate filter. It cannot replace
//! local descriptors because a different pose of the same character can have
//! a large perceptual-hash distance.

const HASH_SIZE: usize = 8;
const DCT_SIZE: usize = 32;

/// Compute a 64-bit DCT perceptual hash from packed RGB pixels.
pub fn compute_rgb(rgb: &[u8], width: u32, height: u32) -> Result<u64, String> {
    let width = usize::try_from(width).map_err(|_| "image width overflows usize".to_owned())?;
    let height = usize::try_from(height).map_err(|_| "image height overflows usize".to_owned())?;
    let expected = width
        .checked_mul(height)
        .and_then(|value| value.checked_mul(3))
        .ok_or_else(|| "image dimensions overflow RGB buffer size".to_owned())?;
    if width == 0 || height == 0 || rgb.len() != expected {
        return Err("RGB buffer dimensions are invalid".to_owned());
    }

    let sample = resize_grayscale(rgb, width, height);
    let coefficients = dct_low_frequency(&sample);
    let mut values = coefficients[1..].to_vec();
    values.sort_by(f32::total_cmp);
    let median = values[values.len() / 2];

    let mut hash = 0_u64;
    for (index, coefficient) in coefficients.iter().enumerate() {
        if *coefficient > median {
            hash |= 1_u64 << index;
        }
    }
    Ok(hash)
}

/// Return the number of different bits between two pHashes.
pub fn hamming_distance(left: u64, right: u64) -> u32 {
    (left ^ right).count_ones()
}

fn resize_grayscale(rgb: &[u8], width: usize, height: usize) -> Vec<f32> {
    let mut output = vec![0.0_f32; DCT_SIZE * DCT_SIZE];
    for target_y in 0..DCT_SIZE {
        let source_y = ((target_y * height) / DCT_SIZE).min(height - 1);
        for target_x in 0..DCT_SIZE {
            let source_x = ((target_x * width) / DCT_SIZE).min(width - 1);
            let offset = (source_y * width + source_x) * 3;
            let red = f32::from(rgb[offset]);
            let green = f32::from(rgb[offset + 1]);
            let blue = f32::from(rgb[offset + 2]);
            output[target_y * DCT_SIZE + target_x] = 0.299 * red + 0.587 * green + 0.114 * blue;
        }
    }
    output
}

fn dct_low_frequency(input: &[f32]) -> [f32; HASH_SIZE * HASH_SIZE] {
    let mut output = [0.0_f32; HASH_SIZE * HASH_SIZE];
    let scale = std::f32::consts::PI / DCT_SIZE as f32;
    for u in 0..HASH_SIZE {
        for v in 0..HASH_SIZE {
            let mut sum = 0.0_f32;
            for x in 0..DCT_SIZE {
                for y in 0..DCT_SIZE {
                    let angle_x = (x as f32 + 0.5) * u as f32 * scale;
                    let angle_y = (y as f32 + 0.5) * v as f32 * scale;
                    sum += input[x * DCT_SIZE + y] * angle_x.cos() * angle_y.cos();
                }
            }
            output[u * HASH_SIZE + v] = sum;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{compute_rgb, hamming_distance};

    fn image(color: [u8; 3], width: usize, height: usize) -> Vec<u8> {
        color.into_iter().cycle().take(width * height * 3).collect()
    }

    #[test]
    fn identical_images_have_zero_distance() {
        let pixels = image([230, 190, 90], 17, 11);
        let left = compute_rgb(&pixels, 17, 11).unwrap();
        let right = compute_rgb(&pixels, 17, 11).unwrap();
        assert_eq!(left, right);
        assert_eq!(hamming_distance(left, right), 0);
    }

    #[test]
    fn different_structure_changes_hash() {
        let mut left = image([230, 190, 90], 32, 32);
        let mut right = left.clone();
        for y in 8..24 {
            for x in 8..24 {
                let offset = (y * 32 + x) * 3;
                right[offset..offset + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        let left_hash = compute_rgb(&left, 32, 32).unwrap();
        let right_hash = compute_rgb(&right, 32, 32).unwrap();
        assert!(hamming_distance(left_hash, right_hash) > 0);
        left[0] = 1;
        assert!(compute_rgb(&left, 32, 32).is_ok());
    }

    #[test]
    fn rejects_inconsistent_buffer() {
        assert!(compute_rgb(&[0, 0, 0], 2, 2).is_err());
        assert!(compute_rgb(&[], 0, 0).is_err());
    }
}
