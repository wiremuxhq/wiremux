# Changelog

## [0.2.0](https://github.com/wiremuxhq/wiremux/compare/v0.1.0...v0.2.0) (2026-09-14)


### Features

* WireClient, shipped key profiles, and crates.io publish ([#77](https://github.com/wiremuxhq/wiremux/issues/77)) ([4150e13](https://github.com/wiremuxhq/wiremux/commit/4150e1356c6e2be0a99b584b87580ae1869120e4))


### Bug Fixes

* **auth:** send Gemini keys as x-goog-api-key ([#82](https://github.com/wiremuxhq/wiremux/issues/82)) ([42086ed](https://github.com/wiremuxhq/wiremux/commit/42086ed3dc4730d0a3003c2747d0f21ca74213f4))
* **ci:** sync wiremux-auth path-dep version on release PRs ([#78](https://github.com/wiremuxhq/wiremux/issues/78)) ([738b597](https://github.com/wiremuxhq/wiremux/commit/738b597bdeee6a47f51369d58b463905e8d1052e)), closes [#75](https://github.com/wiremuxhq/wiremux/issues/75)
* **client:** list_models uses the chat version prefix ([#80](https://github.com/wiremuxhq/wiremux/issues/80)) ([5d82fc1](https://github.com/wiremuxhq/wiremux/commit/5d82fc11bfc16db01f27060470e1430b1973828a))
* **client:** parse Gemini list_models catalog ([#81](https://github.com/wiremuxhq/wiremux/issues/81)) ([b302226](https://github.com/wiremuxhq/wiremux/commit/b302226a6c81c06530d9589286e2e377e642c542))
* **client:** Responses text, stream 200 errors, oat Bearer ([#79](https://github.com/wiremuxhq/wiremux/issues/79)) ([282ef8b](https://github.com/wiremuxhq/wiremux/commit/282ef8b7d46de572d338f42d64e333db1997a348))
* **maps:** emit Anthropic thinking on Messages ([#70](https://github.com/wiremuxhq/wiremux/issues/70)) ([d4dbfee](https://github.com/wiremuxhq/wiremux/commit/d4dbfeed20a2e995e4dc9ff16cfac824fa053660))
* **maps:** emit Chat store, Gemini thinkingLevel, Responses summary ([#76](https://github.com/wiremuxhq/wiremux/issues/76)) ([40109c5](https://github.com/wiremuxhq/wiremux/commit/40109c5437dc567af90489686fc19731d2d6da3c))
* **maps:** remap thinking across Gemini and Responses ([#72](https://github.com/wiremuxhq/wiremux/issues/72)) ([a8bea27](https://github.com/wiremuxhq/wiremux/commit/a8bea27b7a0cd192a99ce95729fc48fe9b73119f))

## [0.1.0](https://github.com/wiremuxhq/wiremux/compare/v0.0.1...v0.1.0) (2026-09-13)


### Features

* add Gemini generateContent maps ([#19](https://github.com/wiremuxhq/wiremux/issues/19)) ([87fe32c](https://github.com/wiremuxhq/wiremux/commit/87fe32cafc86bba6a8960da93dbd2afe5fe98dd5))
* Anthropic cache_control preferred pair and cap ([#25](https://github.com/wiremuxhq/wiremux/issues/25)) ([29bd51c](https://github.com/wiremuxhq/wiremux/commit/29bd51c247243ac73f236cd5d8a91de7ad82e765))
* Application Support overlay and Copilot device gist ([#20](https://github.com/wiremuxhq/wiremux/issues/20)) ([b677a36](https://github.com/wiremuxhq/wiremux/commit/b677a36ed16f92ccf27beded2af8cffed03b4207))
* assemble Chat tool-call id then name across frames ([#33](https://github.com/wiremuxhq/wiremux/issues/33)) ([8630a7f](https://github.com/wiremuxhq/wiremux/commit/8630a7f0aa82d2343bfc4ff930ee2f3b52ceb4a3))
* **auth:** add token_for_profile and Chat o-series sampling ([#60](https://github.com/wiremuxhq/wiremux/issues/60)) ([b8ab040](https://github.com/wiremuxhq/wiremux/commit/b8ab040ff9598afd3c0323b68951efef26fb454f)), closes [#59](https://github.com/wiremuxhq/wiremux/issues/59)
* **auth:** persist OIDC token_url and adopt copilot hosts ([#24](https://github.com/wiremuxhq/wiremux/issues/24)) ([dd56f1d](https://github.com/wiremuxhq/wiremux/commit/dd56f1df73abf71cb585d919c2a84d0a95c67d32))
* **auth:** remove one oidc-auth-json store entry ([#28](https://github.com/wiremuxhq/wiremux/issues/28)) ([3c364cc](https://github.com/wiremuxhq/wiremux/commit/3c364cc5ecfe67afe58f8de461f92269527749d5))
* **auth:** resolve copilot-hosts oauth_token entries ([#18](https://github.com/wiremuxhq/wiremux/issues/18)) ([f6e4579](https://github.com/wiremuxhq/wiremux/commit/f6e4579c3328f8b88b6cb9fac7505e780d53b9f1))
* **auth:** resolve oidc-auth-json named store entries ([#13](https://github.com/wiremuxhq/wiremux/issues/13)) ([918c48c](https://github.com/wiremuxhq/wiremux/commit/918c48c7bd5c0c94750d06d50193567551b05655))
* Chat/Messages/Responses request maps ([#5](https://github.com/wiremuxhq/wiremux/issues/5)) ([6f89dc7](https://github.com/wiremuxhq/wiremux/commit/6f89dc7d0a4521a0bb57cae51c895b9d8df9d650))
* dialect stream maps ([#7](https://github.com/wiremuxhq/wiremux/issues/7)) ([ae11596](https://github.com/wiremuxhq/wiremux/commit/ae11596a5b50716ab6a22ba9dda833d797767d6d))
* emit usage and finish from one message_delta ([#30](https://github.com/wiremuxhq/wiremux/issues/30)) ([e35fce6](https://github.com/wiremuxhq/wiremux/commit/e35fce6f724b9536edc5c5a0d15016dbda90f7c6))
* fan out complete tool calls in decode_stream_events ([#31](https://github.com/wiremuxhq/wiremux/issues/31)) ([50cae49](https://github.com/wiremuxhq/wiremux/commit/50cae49ab9e833ad4b02ba8b7cd25bf3a7d8c1f6))
* Gemini streamGenerateContent URL for SSE ([#21](https://github.com/wiremuxhq/wiremux/issues/21)) ([f35ae9f](https://github.com/wiremuxhq/wiremux/commit/f35ae9f3d900f22e3cd80af8e6ac35c02e62a03f))
* item-centered IR and LossReport ([#4](https://github.com/wiremuxhq/wiremux/issues/4)) ([b1c9abd](https://github.com/wiremuxhq/wiremux/commit/b1c9abd97a8a1d08647381270855b023674f2fe4))
* map Responses reasoning and refusal stream events ([#26](https://github.com/wiremuxhq/wiremux/issues/26)) ([7374bb1](https://github.com/wiremuxhq/wiremux/commit/7374bb17f200ee349bce22126427fb011eb421f0))
* match Bline thinking replay and finish tags ([#23](https://github.com/wiremuxhq/wiremux/issues/23)) ([2cdfc47](https://github.com/wiremuxhq/wiremux/commit/2cdfc472e0576f14bdda7c02ecc4312b01d4711b))
* overlay merge for a single id ([#9](https://github.com/wiremuxhq/wiremux/issues/9)) ([644a773](https://github.com/wiremuxhq/wiremux/commit/644a7737ef66c459ae54a9eecd406cb0434367ac))
* own remaining LLM TokenProviders and cache floor ([#48](https://github.com/wiremuxhq/wiremux/issues/48)) ([1220db0](https://github.com/wiremuxhq/wiremux/commit/1220db02dce1e8c2928a78fedad668397495e7e8))
* profile schema v1, catalog by id, data-only validation ([#3](https://github.com/wiremuxhq/wiremux/issues/3)) ([29b8185](https://github.com/wiremuxhq/wiremux/commit/29b81855337b8d635e67c91520a1f16ff6f7a6e0))
* profile-driven TokenProvider with fail-closed write-back ([#6](https://github.com/wiremuxhq/wiremux/issues/6)) ([ffcc4e8](https://github.com/wiremuxhq/wiremux/commit/ffcc4e863c4f18446ebc071d93b81017b17fcd74))
* **proxy:** keep stream:true for Grok always-SSE ([#15](https://github.com/wiremuxhq/wiremux/issues/15)) ([44a2246](https://github.com/wiremuxhq/wiremux/commit/44a2246d214f0473c3ed6f04c8ba30d53b0b6902))
* **proxy:** remap SSE frames as they arrive ([#17](https://github.com/wiremuxhq/wiremux/issues/17)) ([391d5d6](https://github.com/wiremuxhq/wiremux/commit/391d5d6b3a89538cf8b978ff4a287c6250f807ac))
* reasoning sampling and consume leftover align ([#41](https://github.com/wiremuxhq/wiremux/issues/41)) ([ebe1f5d](https://github.com/wiremuxhq/wiremux/commit/ebe1f5d3b378efbe5fadf7e363da0139d6121a5f))
* replay Gemini thoughtSignature and thinkingConfig ([#22](https://github.com/wiremuxhq/wiremux/issues/22)) ([6060a07](https://github.com/wiremuxhq/wiremux/commit/6060a07b30a39f73db2c5edd84a5c5f32fb7d6b3))
* ship anthropic-oauth, openai-codex-oauth, openrouter-codex, grok-ollama ([#8](https://github.com/wiremuxhq/wiremux/issues/8)) ([8ae43b2](https://github.com/wiremuxhq/wiremux/commit/8ae43b234a12177c20bbcf272bab2cc7a9a793f3))
* wiremux CLI proxy and profile validate ([#11](https://github.com/wiremuxhq/wiremux/issues/11)) ([4cb9673](https://github.com/wiremuxhq/wiremux/commit/4cb96737e38374e87365922f95b3da9ba29c7327))


### Bug Fixes

* **auth:** keep wire fallback off shipped presets ([#50](https://github.com/wiremuxhq/wiremux/issues/50)) ([f72ae39](https://github.com/wiremuxhq/wiremux/commit/f72ae39f34a1a5eafc94da5fbff037eaef7df870))
* **auth:** redact envsubst values in profile errors ([#55](https://github.com/wiremuxhq/wiremux/issues/55)) ([5a58a39](https://github.com/wiremuxhq/wiremux/commit/5a58a39e6aa26b8dff58372d3790734e2409d892)), closes [#51](https://github.com/wiremuxhq/wiremux/issues/51)
* Bline consume leftovers (cache_control, optional deps, notes) ([#37](https://github.com/wiremuxhq/wiremux/issues/37)) ([4d939fe](https://github.com/wiremuxhq/wiremux/commit/4d939fe87cdf0a3ce9eba599810520eeaf55b6b9))
* keep Gemini functionCall args when thoughtSignature is set ([#27](https://github.com/wiremuxhq/wiremux/issues/27)) ([aa86f4f](https://github.com/wiremuxhq/wiremux/commit/aa86f4f5f784c9605b3c1ccae66b7d3e65bc03a0))
* map loss, auth errors, and hostile-input fail-closed ([#43](https://github.com/wiremuxhq/wiremux/issues/43)) ([3f60a04](https://github.com/wiremuxhq/wiremux/commit/3f60a0437b8407cb10f37d706ff7c112a9a4d6a4))
* map loss, Gemini thinkingConfig, and auth fail-closed ([#47](https://github.com/wiremuxhq/wiremux/issues/47)) ([8db1e29](https://github.com/wiremuxhq/wiremux/commit/8db1e2988ef1d79450e52f360e0da19b3fbbdb72))
* **maps:** Chat o-series decode and consume helper errors ([#61](https://github.com/wiremuxhq/wiremux/issues/61)) ([fd4ff89](https://github.com/wiremuxhq/wiremux/commit/fd4ff891738c2caaffab3eaa6b7a6a2646903fb6))
* **maps:** Gemini tool_choice and leftover stream holes ([#58](https://github.com/wiremuxhq/wiremux/issues/58)) ([6a77c4d](https://github.com/wiremuxhq/wiremux/commit/6a77c4db30e0bbbe525fc1eb67e864c669133820))
* **maps:** report include drop off Responses ([#63](https://github.com/wiremuxhq/wiremux/issues/63)) ([7d4904a](https://github.com/wiremuxhq/wiremux/commit/7d4904a76136300a3a949b4a8e7e00c9224789cd))
* **stream:** exclusive usage buckets like Bline ([#16](https://github.com/wiremuxhq/wiremux/issues/16)) ([0c800a8](https://github.com/wiremuxhq/wiremux/commit/0c800a80b054e2ac757d015ec4672e6e3798c673))
* **stream:** fan-out Chat/Responses and Gemini json_schema ([#56](https://github.com/wiremuxhq/wiremux/issues/56)) ([7a1b70e](https://github.com/wiremuxhq/wiremux/commit/7a1b70e5991eb1b14353e437570d69c780be7fab)), closes [#52](https://github.com/wiremuxhq/wiremux/issues/52) [#53](https://github.com/wiremuxhq/wiremux/issues/53) [#54](https://github.com/wiremuxhq/wiremux/issues/54)
* **stream:** invert Responses finish encode ([#64](https://github.com/wiremuxhq/wiremux/issues/64)) ([f72530f](https://github.com/wiremuxhq/wiremux/commit/f72530f416f8e53e153623970554bc84889f48a1))
* **stream:** remap finish reasons and keep file parts ([#62](https://github.com/wiremuxhq/wiremux/issues/62)) ([c5731d7](https://github.com/wiremuxhq/wiremux/commit/c5731d78ea06ce364c0fc0b2e864ddf2234eb1c6))
