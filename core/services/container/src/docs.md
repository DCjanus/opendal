## Capabilities

This experimental service can be used to:

- [ ] create_dir
- [x] stat
- [x] read
- [ ] write
- [ ] delete
- [x] list
- [ ] copy
- [ ] rename
- [ ] presign

## Notes

This service currently supports local OCI image layouts only. It resolves a
single image reference and exposes the merged rootfs as a read-only OpenDAL
operator.

The service supports uncompressed tar and gzip-compressed tar layers. It
applies OCI whiteouts and filesystem replacement rules while merging layers,
and resolves symbolic and hard links within the image rootfs for `stat` and
`read` operations.

The initial implementation builds an in-memory index from layer tar archives
while the operator is being created. Remote registries, Docker daemon storage,
Podman or containerd storage, OCI archives, Docker archives, per-layer access,
attestations, annotations, and SBOMs are not supported yet.

## Configuration

- `layout`: Local OCI image layout directory.
- `reference`: Image reference name in `index.json`.
- `platform`: Image platform selector, for example `linux/amd64`.
- `root`: Root path inside the merged image rootfs. Defaults to `/`.

You can refer to [`ContainerBuilder`]'s docs for more information.
