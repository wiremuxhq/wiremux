# Changelog

## [0.6.0](https://github.com/wiremuxhq/wiremux/compare/v0.5.0...v0.6.0) (2026-09-16)


### Features

* **aws:** resolve SSO, credential_process, ECS, and IMDS ([#167](https://github.com/wiremuxhq/wiremux/issues/167)) ([7b3d345](https://github.com/wiremuxhq/wiremux/commit/7b3d345b9f8f80524511f00c7e2b59867abe70cb)), closes [#153](https://github.com/wiremuxhq/wiremux/issues/153)
* mark Wire non-exhaustive and honor Converse tool none ([#144](https://github.com/wiremuxhq/wiremux/issues/144)) ([0a752a7](https://github.com/wiremuxhq/wiremux/commit/0a752a78585e25fa86abe7d69ddc12fa0cffb6e9))
* stream grammar, AWS sign, presets, and IR extras ([#165](https://github.com/wiremuxhq/wiremux/issues/165)) ([0043d0b](https://github.com/wiremuxhq/wiremux/commit/0043d0b0bd593fea28597194fc45db13811c9230))


### Bug Fixes

* emit Converse outputConfig for schema and effort ([#145](https://github.com/wiremuxhq/wiremux/issues/145)) ([0e93db1](https://github.com/wiremuxhq/wiremux/commit/0e93db1fb6d038c07eccb6f3a0195cbc85adeded))
* emit Converse serviceTier for mapped service_tier ([#146](https://github.com/wiremuxhq/wiremux/issues/146)) ([c2e8451](https://github.com/wiremuxhq/wiremux/commit/c2e84516810c960a14632955962e65cc2fbcf9db))
* encode model URL segments and reject proxy CSRF ([#164](https://github.com/wiremuxhq/wiremux/issues/164)) ([0532019](https://github.com/wiremuxhq/wiremux/commit/05320197c4d5ea2b1f99bb640f9ebc058927f358))
* keep maps-only path-dep extras when syncing versions ([#166](https://github.com/wiremuxhq/wiremux/issues/166)) ([1d9b4e9](https://github.com/wiremuxhq/wiremux/commit/1d9b4e92746ff008c7fde26eb4ec419030eb492f))
* pin consume notes 0.5.0 and report Converse sampling loss ([#140](https://github.com/wiremuxhq/wiremux/issues/140)) ([28876b1](https://github.com/wiremuxhq/wiremux/commit/28876b1317c4af64633e673fe6e9780b555e01db))
* read context_window from OpenAI-compat /models ([#143](https://github.com/wiremuxhq/wiremux/issues/143)) ([6f2de75](https://github.com/wiremuxhq/wiremux/commit/6f2de751133bb65dc1d4d0b742581209be55100f)), closes [#142](https://github.com/wiremuxhq/wiremux/issues/142)

## [0.5.0](https://github.com/wiremuxhq/wiremux/compare/v0.4.0...v0.5.0) (2026-09-16)


### Features

* accept gcloud authorized_user ADC for Vertex ([#130](https://github.com/wiremuxhq/wiremux/issues/130)) ([a222e55](https://github.com/wiremuxhq/wiremux/commit/a222e55635db84dcdf3a1bb39361dde971d1aef1))
* ingest catalog vendors into user-dir profiles ([#124](https://github.com/wiremuxhq/wiremux/issues/124)) ([b46840a](https://github.com/wiremuxhq/wiremux/commit/b46840a36202b0418ffffd7658fcf74afa227405))
* ingest Messages hosts, Azure/Vertex URLs, Bedrock Converse ([#125](https://github.com/wiremuxhq/wiremux/issues/125)) ([62d6f9a](https://github.com/wiremuxhq/wiremux/commit/62d6f9a13c4b2a1daacde7770db811e3b6f49b81))
* map Codex prompt_cache_key and service_tier ([#123](https://github.com/wiremuxhq/wiremux/issues/123)) ([1042510](https://github.com/wiremuxhq/wiremux/commit/1042510b822f123a46146ca1040a5c3a59b0348f))
* Messages Grok Build catalog, Continue., and shipped ids ([#120](https://github.com/wiremuxhq/wiremux/issues/120)) ([64fd644](https://github.com/wiremuxhq/wiremux/commit/64fd644ab5b2efd9ddf1f4c0b4a4671ba106552b)), closes [#114](https://github.com/wiremuxhq/wiremux/issues/114) [#115](https://github.com/wiremuxhq/wiremux/issues/115) [#116](https://github.com/wiremuxhq/wiremux/issues/116) [#117](https://github.com/wiremuxhq/wiremux/issues/117) [#118](https://github.com/wiremuxhq/wiremux/issues/118) [#119](https://github.com/wiremuxhq/wiremux/issues/119)
* Vertex GCP key file and Bedrock Event Stream ([#129](https://github.com/wiremuxhq/wiremux/issues/129)) ([b98ecd0](https://github.com/wiremuxhq/wiremux/commit/b98ecd086a2297a1f82f31801504f5b33d123f76))


### Bug Fixes

* emit Messages tool required as an array ([#122](https://github.com/wiremuxhq/wiremux/issues/122)) ([4b990c1](https://github.com/wiremuxhq/wiremux/commit/4b990c1c014fb7ae67bd51280207dcd443128173))
* Event Stream AWS payload shape, exceptions, and ADC quota project ([#131](https://github.com/wiremuxhq/wiremux/issues/131)) ([577395b](https://github.com/wiremuxhq/wiremux/commit/577395b5fa4f3c67927dc10fea45a7ceb9f32ae9))
* group Converse toolResults and keep stopReason ([#128](https://github.com/wiremuxhq/wiremux/issues/128)) ([f58f9e3](https://github.com/wiremuxhq/wiremux/commit/f58f9e3a47a1f16eec4c77818e8dcda58ff9f171))
* harden ingest all-compatible and Converse body ([#126](https://github.com/wiremuxhq/wiremux/issues/126)) ([7996461](https://github.com/wiremuxhq/wiremux/commit/7996461cb50f8bdd046a8e7c8458390c1f1a4c67))
* keep Converse mixed assistant text with toolUse ([#127](https://github.com/wiremuxhq/wiremux/issues/127)) ([163a458](https://github.com/wiremuxhq/wiremux/commit/163a4587aa08974aea119618bdc5561bf73c2056))
* keep vendor error text and surface stream exceptions ([#133](https://github.com/wiremuxhq/wiremux/issues/133)) ([6145abd](https://github.com/wiremuxhq/wiremux/commit/6145abd6352b43564e3743ed1b9b6325763f6cf5))
* send proxy decode errors as SSE data frames ([#134](https://github.com/wiremuxhq/wiremux/issues/134)) ([b18ad5b](https://github.com/wiremuxhq/wiremux/commit/b18ad5bac67672934d4eb7aaed47cde25d3483fd))
* send proxy upstream stream errors as SSE data frames ([#135](https://github.com/wiremuxhq/wiremux/issues/135)) ([893793a](https://github.com/wiremuxhq/wiremux/commit/893793a866c0f783385bf511776a5d96ee6165de))
* send same-dialect proxy stream errors as SSE data frames ([#136](https://github.com/wiremuxhq/wiremux/issues/136)) ([d8f3486](https://github.com/wiremuxhq/wiremux/commit/d8f3486d9810eae7e336d1b77c41aa9290aadbfa))
* Vertex global host, streamRawPredict, and Event Stream errors ([#132](https://github.com/wiremuxhq/wiremux/issues/132)) ([00688bf](https://github.com/wiremuxhq/wiremux/commit/00688bff87431bc40a4b7b9a5f62fef5416d3fc4))

## [0.4.0](https://github.com/wiremuxhq/wiremux/compare/v0.3.0...v0.4.0) (2026-09-15)


### Features

* ship xai-grok-build Grok Build CLI proxy profile ([#108](https://github.com/wiremuxhq/wiremux/issues/108)) ([022f229](https://github.com/wiremuxhq/wiremux/commit/022f22907ec63ddb84d3216d39683a8169d4acb6)), closes [#104](https://github.com/wiremuxhq/wiremux/issues/104)


### Bug Fixes

* fail fast on expired oauth refresh transport ([#105](https://github.com/wiremuxhq/wiremux/issues/105)) ([46ec132](https://github.com/wiremuxhq/wiremux/commit/46ec13250c9f893675584b268caec2ec77e402a1))
* map cross-dialect chat proxy responses ([#111](https://github.com/wiremuxhq/wiremux/issues/111)) ([feba1a0](https://github.com/wiremuxhq/wiremux/commit/feba1a0eb4757df8e41ff3a50df6d80ee3f4b0d3))
* map non-stream Chat bodies to all client wires ([#113](https://github.com/wiremuxhq/wiremux/issues/113)) ([8c793fa](https://github.com/wiremuxhq/wiremux/commit/8c793fabd89abd08bc084b08598b665a3cce406a))
* prefer edit distance for catalog id did-you-mean ([#110](https://github.com/wiremuxhq/wiremux/issues/110)) ([091994c](https://github.com/wiremuxhq/wiremux/commit/091994c4cfedb283ed534dd2c61dc1e51e570ccc))
* Protocol slot, login hints, and proxy body cap ([#112](https://github.com/wiremuxhq/wiremux/issues/112)) ([aff90ff](https://github.com/wiremuxhq/wiremux/commit/aff90ffddc9a8f682d656da742ca4dc1f9c9b993))
* send Grok CLI version header on xai-grok-build ([#109](https://github.com/wiremuxhq/wiremux/issues/109)) ([9dd6558](https://github.com/wiremuxhq/wiremux/commit/9dd65582052f4cd4a49abd7d697427ddb23480c4)), closes [#104](https://github.com/wiremuxhq/wiremux/issues/104)

## [0.3.0](https://github.com/wiremuxhq/wiremux/compare/v0.2.1...v0.3.0) (2026-09-14)


### Features

* load Grok OIDC from ~/.grok/auth.json ([#100](https://github.com/wiremuxhq/wiremux/issues/100)) ([b45398a](https://github.com/wiremuxhq/wiremux/commit/b45398a13a77a9ad937402b3e3f90c860d50342a))


### Bug Fixes

* classify stream leftovers and redact ClientError Debug ([#92](https://github.com/wiremuxhq/wiremux/issues/92)) ([e2bfae2](https://github.com/wiremuxhq/wiremux/commit/e2bfae22aab7e6ad92d5a2d6f23b8e3cd8cce48f))
* passthrough same-dialect SSE ([#99](https://github.com/wiremuxhq/wiremux/issues/99)) ([8effae2](https://github.com/wiremuxhq/wiremux/commit/8effae2eaf0ec77b61677c64f832d97d59a6a965))
* refuse nested profile tables and name schema_version ([#94](https://github.com/wiremuxhq/wiremux/issues/94)) ([acbdf2a](https://github.com/wiremuxhq/wiremux/commit/acbdf2a9e2cce88c857d5f8bbce5207ce235ebfd))
* send JSON Content-Type and honor access_env ([#97](https://github.com/wiremuxhq/wiremux/issues/97)) ([ca582a4](https://github.com/wiremuxhq/wiremux/commit/ca582a4dc7124d2fa8eaf226694eb3f44da14326))
* try login account before shipped keychain names ([#96](https://github.com/wiremuxhq/wiremux/issues/96)) ([7475a3b](https://github.com/wiremuxhq/wiremux/commit/7475a3b0ea37264051fb4c2d7c375a7aac6c6fd1)), closes [#95](https://github.com/wiremuxhq/wiremux/issues/95)

## [0.2.1](https://github.com/wiremuxhq/wiremux/compare/v0.2.0...v0.2.1) (2026-09-14)


### Bug Fixes

* **auth:** vendor shipped presets inside the crate ([#83](https://github.com/wiremuxhq/wiremux/issues/83)) ([ebde729](https://github.com/wiremuxhq/wiremux/commit/ebde7293a2dc1c9de0e566119b92b71da4a83b4f))

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
