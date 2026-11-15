# platform

The platform is built from the modules in `modules/`, one `.mod` file
each, by `build.py`, which reads its settings from `build.cfg`:

- `[toolchain] api_level`: the API level to build for. Every module declares
  the range of levels it supports, and so does every library version.
- `[features] enabled`: the optional features compiled in. A module can
  only use enabled features, and every enabled feature must be used.
- `[aliases]`: which version of each library a short name such as `store`
  stands for. Modules call libraries by their short names.

The build compiles, links, and packages; it stops after the first phase that
finds a problem.

```sh
python3 build.py
```
