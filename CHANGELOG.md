# Changelog

## [1.0.8](https://github.com/grafana/pyroscope-python/compare/python-1.0.7...python-1.0.8) (2026-04-24)


### Bug Fixes

* revert 0976d999e4a50c14459b8a0b39e72434be6d4bfc     ([#483](https://github.com/grafana/pyroscope-python/issues/483)) ([ad3d032](https://github.com/grafana/pyroscope-python/commit/ad3d03217a14a18868b8ba21f3dbf4ed844d759a))
* set --openssldir=/etc/ssl so profiler finds system CA bundle ([#18](https://github.com/grafana/pyroscope-python/issues/18)) ([f124c26](https://github.com/grafana/pyroscope-python/commit/f124c2637a6aaf55d4b6f452a1ef2bb26cfd5e65))
* strip trailing comments when extracting Cargo.toml version ([#26](https://github.com/grafana/pyroscope-python/issues/26)) ([7ba547b](https://github.com/grafana/pyroscope-python/commit/7ba547bb8054dff22c0434ec965af5772f199136))


### Miscellaneous Chores

* **deps:** bump openssl from 0.10.76 to 0.10.78 in /rust ([#49](https://github.com/grafana/pyroscope-python/issues/49)) ([051156b](https://github.com/grafana/pyroscope-python/commit/051156b46ea1c1ec2ba65b69e9a75b9e4d19a5f1))
* **deps:** bump rand from 0.8.5 to 0.8.6 in /rust ([#53](https://github.com/grafana/pyroscope-python/issues/53)) ([cad19aa](https://github.com/grafana/pyroscope-python/commit/cad19aacb9244a5ad3189dd08981192242ea0d6c))
* **deps:** bump rustls-webpki from 0.103.10 to 0.103.13 in /rust ([#52](https://github.com/grafana/pyroscope-python/issues/52)) ([5f638b0](https://github.com/grafana/pyroscope-python/commit/5f638b0ddd0b1f0e5b8ad709aaf1453db8b76545))
* **deps:** lock file maintenance ([#22](https://github.com/grafana/pyroscope-python/issues/22)) ([634ae96](https://github.com/grafana/pyroscope-python/commit/634ae960c433d7186e28a89059af58c726cce6f9))
* **deps:** lock file maintenance ([#27](https://github.com/grafana/pyroscope-python/issues/27)) ([91a81f8](https://github.com/grafana/pyroscope-python/commit/91a81f80a758705587700600a2e842b53f4393c9))
* **deps:** lock file maintenance ([#46](https://github.com/grafana/pyroscope-python/issues/46)) ([d67a599](https://github.com/grafana/pyroscope-python/commit/d67a5991fcdb9facbc1b5a2457bb87deee3dd9d9))
* **deps:** update actions/download-artifact action to v8.0.1 ([#6](https://github.com/grafana/pyroscope-python/issues/6)) ([4956a50](https://github.com/grafana/pyroscope-python/commit/4956a50864c92b2b77b99a0d3a8c868df81dbf5b))
* **deps:** update actions/upload-artifact action to v7.0.1 ([#31](https://github.com/grafana/pyroscope-python/issues/31)) ([65ecf12](https://github.com/grafana/pyroscope-python/commit/65ecf127cb88ebbcadae1e1c230d7adbb466e2d1))
* **deps:** update dependency openssl/openssl to v3.5.6 ([#24](https://github.com/grafana/pyroscope-python/issues/24)) ([2ab293a](https://github.com/grafana/pyroscope-python/commit/2ab293a5627bbe719d0f8be6db211486e83f1760))
* **deps:** update docker/dockerfile docker tag to v1.23 ([#19](https://github.com/grafana/pyroscope-python/issues/19)) ([7242236](https://github.com/grafana/pyroscope-python/commit/72422366990dde992476b24bee43b89dae22ccbc))
* **deps:** update dtolnay/rust-toolchain digest to 3c5f7ea ([#21](https://github.com/grafana/pyroscope-python/issues/21)) ([49ece8d](https://github.com/grafana/pyroscope-python/commit/49ece8d30f52f91aae5f91aa2446a0473cda1f4d))
* **deps:** update dtolnay/rust-toolchain digest to 3c5f7ea ([#3](https://github.com/grafana/pyroscope-python/issues/3)) ([d7a3c82](https://github.com/grafana/pyroscope-python/commit/d7a3c82ec2e16d908557d3a7147280baebd8a614))
* **deps:** update googleapis/release-please-action digest to 5c625bf ([#30](https://github.com/grafana/pyroscope-python/issues/30)) ([030ec1d](https://github.com/grafana/pyroscope-python/commit/030ec1df73abbcfe3097a4062c0a5ff58f3205e0))
* **deps:** update pypa/gh-action-pypi-publish action to v1.14.0 ([#25](https://github.com/grafana/pyroscope-python/issues/25)) ([5266b25](https://github.com/grafana/pyroscope-python/commit/5266b25cc7172adc924385c183d2ef6364703d72))
* **deps:** update robinraju/release-downloader action to v1.13 ([#47](https://github.com/grafana/pyroscope-python/issues/47)) ([5cf2b7f](https://github.com/grafana/pyroscope-python/commit/5cf2b7f0d9fdbc4890e778a15d7995615f816077))
* **deps:** update rust crate libc to v0.2.185 ([#34](https://github.com/grafana/pyroscope-python/issues/34)) ([0a007be](https://github.com/grafana/pyroscope-python/commit/0a007be9bc1bbe1e926464361bb8b621d01602f9))
* **deps:** update rust crate pyroscope to v2.0.1 ([#48](https://github.com/grafana/pyroscope-python/issues/48)) ([ae46c4d](https://github.com/grafana/pyroscope-python/commit/ae46c4d675110437867496baf43100e20373962d))
* **deps:** update softprops/action-gh-release action to v2.6.1 ([#13](https://github.com/grafana/pyroscope-python/issues/13)) ([46a8787](https://github.com/grafana/pyroscope-python/commit/46a87878ea26b8145085dbcb2a7198d597125a20))
* **deps:** update softprops/action-gh-release action to v3 ([#33](https://github.com/grafana/pyroscope-python/issues/33)) ([bbdf9ac](https://github.com/grafana/pyroscope-python/commit/bbdf9acf2ca5fa8193945e81aee3c38b04eb64db))
* **main:** release python 1.0.5 ([#20](https://github.com/grafana/pyroscope-python/issues/20)) ([9b1b964](https://github.com/grafana/pyroscope-python/commit/9b1b964241485249d5342a1d1e6ef277fce729ad))
* **main:** release python 1.0.6 ([#50](https://github.com/grafana/pyroscope-python/issues/50)) ([594c8fc](https://github.com/grafana/pyroscope-python/commit/594c8fca811032aa77b9851c65e3a9ba5406edf7))
* **main:** release python 1.0.7 ([#51](https://github.com/grafana/pyroscope-python/issues/51)) ([9ac7f65](https://github.com/grafana/pyroscope-python/commit/9ac7f65eaf0e0e9e79f4f07f020f48624b68ead2))
* **python:** keep only python in this repo ([#1](https://github.com/grafana/pyroscope-python/issues/1)) ([a781b81](https://github.com/grafana/pyroscope-python/commit/a781b81963f99aa7030621f9c36308fce9cd1b47))
* switch PyPI publishing to trusted publishing (OIDC) ([#29](https://github.com/grafana/pyroscope-python/issues/29)) ([991156a](https://github.com/grafana/pyroscope-python/commit/991156aa51eab62328a55344fe9ecefe2741ff82))


### Continuous Integration

* add release-please configuration ([#14](https://github.com/grafana/pyroscope-python/issues/14)) ([c6be862](https://github.com/grafana/pyroscope-python/commit/c6be862b19a6bbc2f1a5b21743213e1c8046ac45))

## [1.0.7](https://github.com/grafana/pyroscope-python/compare/python-1.0.6...python-1.0.7) (2026-04-24)


### Miscellaneous Chores

* **deps:** bump rand from 0.8.5 to 0.8.6 in /rust ([#53](https://github.com/grafana/pyroscope-python/issues/53)) ([cad19aa](https://github.com/grafana/pyroscope-python/commit/cad19aacb9244a5ad3189dd08981192242ea0d6c))
* **deps:** bump rustls-webpki from 0.103.10 to 0.103.13 in /rust ([#52](https://github.com/grafana/pyroscope-python/issues/52)) ([5f638b0](https://github.com/grafana/pyroscope-python/commit/5f638b0ddd0b1f0e5b8ad709aaf1453db8b76545))
* **deps:** update rust crate pyroscope to v2.0.1 ([#48](https://github.com/grafana/pyroscope-python/issues/48)) ([ae46c4d](https://github.com/grafana/pyroscope-python/commit/ae46c4d675110437867496baf43100e20373962d))

## [1.0.6](https://github.com/grafana/pyroscope-python/compare/python-1.0.5...python-1.0.6) (2026-04-23)


### Miscellaneous Chores

* **deps:** bump openssl from 0.10.76 to 0.10.78 in /rust ([#49](https://github.com/grafana/pyroscope-python/issues/49)) ([051156b](https://github.com/grafana/pyroscope-python/commit/051156b46ea1c1ec2ba65b69e9a75b9e4d19a5f1))
* **deps:** lock file maintenance ([#46](https://github.com/grafana/pyroscope-python/issues/46)) ([d67a599](https://github.com/grafana/pyroscope-python/commit/d67a5991fcdb9facbc1b5a2457bb87deee3dd9d9))
* **deps:** update actions/upload-artifact action to v7.0.1 ([#31](https://github.com/grafana/pyroscope-python/issues/31)) ([65ecf12](https://github.com/grafana/pyroscope-python/commit/65ecf127cb88ebbcadae1e1c230d7adbb466e2d1))
* **deps:** update googleapis/release-please-action digest to 5c625bf ([#30](https://github.com/grafana/pyroscope-python/issues/30)) ([030ec1d](https://github.com/grafana/pyroscope-python/commit/030ec1df73abbcfe3097a4062c0a5ff58f3205e0))
* **deps:** update robinraju/release-downloader action to v1.13 ([#47](https://github.com/grafana/pyroscope-python/issues/47)) ([5cf2b7f](https://github.com/grafana/pyroscope-python/commit/5cf2b7f0d9fdbc4890e778a15d7995615f816077))
* **deps:** update rust crate libc to v0.2.185 ([#34](https://github.com/grafana/pyroscope-python/issues/34)) ([0a007be](https://github.com/grafana/pyroscope-python/commit/0a007be9bc1bbe1e926464361bb8b621d01602f9))
* **deps:** update softprops/action-gh-release action to v3 ([#33](https://github.com/grafana/pyroscope-python/issues/33)) ([bbdf9ac](https://github.com/grafana/pyroscope-python/commit/bbdf9acf2ca5fa8193945e81aee3c38b04eb64db))
* switch PyPI publishing to trusted publishing (OIDC) ([#29](https://github.com/grafana/pyroscope-python/issues/29)) ([991156a](https://github.com/grafana/pyroscope-python/commit/991156aa51eab62328a55344fe9ecefe2741ff82))

## [1.0.5](https://github.com/grafana/pyroscope-python/compare/python-1.0.4...python-1.0.5) (2026-04-13)


### Bug Fixes

* revert 0976d999e4a50c14459b8a0b39e72434be6d4bfc     ([#483](https://github.com/grafana/pyroscope-python/issues/483)) ([ad3d032](https://github.com/grafana/pyroscope-python/commit/ad3d03217a14a18868b8ba21f3dbf4ed844d759a))
* set --openssldir=/etc/ssl so profiler finds system CA bundle ([#18](https://github.com/grafana/pyroscope-python/issues/18)) ([f124c26](https://github.com/grafana/pyroscope-python/commit/f124c2637a6aaf55d4b6f452a1ef2bb26cfd5e65))


### Miscellaneous Chores

* **deps:** lock file maintenance ([#22](https://github.com/grafana/pyroscope-python/issues/22)) ([634ae96](https://github.com/grafana/pyroscope-python/commit/634ae960c433d7186e28a89059af58c726cce6f9))
* **deps:** update actions/download-artifact action to v8.0.1 ([#6](https://github.com/grafana/pyroscope-python/issues/6)) ([4956a50](https://github.com/grafana/pyroscope-python/commit/4956a50864c92b2b77b99a0d3a8c868df81dbf5b))
* **deps:** update dependency openssl/openssl to v3.5.6 ([#24](https://github.com/grafana/pyroscope-python/issues/24)) ([2ab293a](https://github.com/grafana/pyroscope-python/commit/2ab293a5627bbe719d0f8be6db211486e83f1760))
* **deps:** update docker/dockerfile docker tag to v1.23 ([#19](https://github.com/grafana/pyroscope-python/issues/19)) ([7242236](https://github.com/grafana/pyroscope-python/commit/72422366990dde992476b24bee43b89dae22ccbc))
* **deps:** update dtolnay/rust-toolchain digest to 3c5f7ea ([#21](https://github.com/grafana/pyroscope-python/issues/21)) ([49ece8d](https://github.com/grafana/pyroscope-python/commit/49ece8d30f52f91aae5f91aa2446a0473cda1f4d))
* **deps:** update dtolnay/rust-toolchain digest to 3c5f7ea ([#3](https://github.com/grafana/pyroscope-python/issues/3)) ([d7a3c82](https://github.com/grafana/pyroscope-python/commit/d7a3c82ec2e16d908557d3a7147280baebd8a614))
* **deps:** update softprops/action-gh-release action to v2.6.1 ([#13](https://github.com/grafana/pyroscope-python/issues/13)) ([46a8787](https://github.com/grafana/pyroscope-python/commit/46a87878ea26b8145085dbcb2a7198d597125a20))
* **python:** keep only python in this repo ([#1](https://github.com/grafana/pyroscope-python/issues/1)) ([a781b81](https://github.com/grafana/pyroscope-python/commit/a781b81963f99aa7030621f9c36308fce9cd1b47))


### Continuous Integration

* add release-please configuration ([#14](https://github.com/grafana/pyroscope-python/issues/14)) ([c6be862](https://github.com/grafana/pyroscope-python/commit/c6be862b19a6bbc2f1a5b21743213e1c8046ac45))
