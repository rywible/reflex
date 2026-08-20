//! Held-out split — groups neighboring scene/camera cases to prevent leakage.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitGroup {
    Train,
    HeldOut,
}

/// Scene/camera id encoded in kernel_id prefix: `scene-{n}-cam-{m}`.
pub fn split_group_for_kernel(kernel_id: &str) -> Result<SplitGroup, String> {
    let scene = extract_scene_id(kernel_id)?;
    // Neighboring scenes share floor(scene/3); alternate complete groups so
    // no adjacent three-scene family straddles train and held-out.
    if (scene / 3) % 2 == 0 {
        Ok(SplitGroup::Train)
    } else {
        Ok(SplitGroup::HeldOut)
    }
}

fn extract_scene_id(kernel_id: &str) -> Result<u32, String> {
    let mut parts = kernel_id.split('-');
    match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (Some("scene"), Some(scene), Some("cam"), Some(camera), None)
            if !camera.is_empty() && camera.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            let _: u32 = camera
                .parse()
                .map_err(|_| "camera identifier is not a valid u32".to_string())?;
            scene
                .parse()
                .map_err(|_| "scene identifier is not a valid u32".to_string())
        }
        _ => Err("kernel id must be exactly `scene-{u32}-cam-{u32}`".into()),
    }
}

pub fn is_held_out(kernel_id: &str) -> Result<bool, String> {
    Ok(matches!(
        split_group_for_kernel(kernel_id)?,
        SplitGroup::HeldOut
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_held_out_split() {
        assert!(!is_held_out("scene-0-cam-0").unwrap());
        assert!(is_held_out("scene-5-cam-1").unwrap());
    }

    #[test]
    fn test_neighboring_scenes_same_group() {
        assert_eq!(
            split_group_for_kernel("scene-0-cam-0").unwrap(),
            split_group_for_kernel("scene-1-cam-0").unwrap()
        );
        assert_eq!(
            split_group_for_kernel("scene-3-cam-0").unwrap(),
            split_group_for_kernel("scene-5-cam-9").unwrap()
        );
    }

    #[test]
    fn malformed_kernel_never_falls_into_a_registered_split() {
        for malformed in ["", "kernel-a", "scene-x-cam-0", "scene-1-camera-0"] {
            assert!(split_group_for_kernel(malformed).is_err());
        }
    }
}
