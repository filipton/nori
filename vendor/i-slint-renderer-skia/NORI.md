Based on i-slint-renderer-skia 1.18.1 from crates.io. Upstream licenses are in LICENSES.

Changes marked NORI expose owned Skia image conversion and the wgpu 30 resource-cache budget.
These avoid duplicate raster buffers and bound retained GPU resources without borrowed Vulkan cover images.

This crate is a workspace member so Bazel resolves its local source without absolute path repositories.
Two style lints allow the existing upstream rendering callback signatures.
