# Overlays

Each `*.yaml` here is an [OpenAPI Overlay 1.0](https://spec.openapis.org/overlay/v1.0.0.html)
document applied to `../asana_oas.yaml` before generation, in file-name order. An overlay holds one
correction, with a comment saying what the published spec gets wrong and how that was established.
The vendored spec itself is never edited. An action may state `x-expect`, the keys its target must
hold (`null`: must not hold) for the correction to still be needed; `docs/generating.md` says when
generation fails.
