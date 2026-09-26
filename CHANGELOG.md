# Changelog

## [0.9.2](https://github.com/wiremuxhq/wiremux/compare/v0.9.1...v0.9.2) (2026-09-26)


### Bug Fixes

* decode Chat stream custom tools and reject scalar arguments ([#303](https://github.com/wiremuxhq/wiremux/issues/303)) ([7727ac0](https://github.com/wiremuxhq/wiremux/commit/7727ac03283ff64f3cce695ffa867292c0a7b079)), closes [#301](https://github.com/wiremuxhq/wiremux/issues/301) [#302](https://github.com/wiremuxhq/wiremux/issues/302)
* decode Gemini response images into IR events ([#307](https://github.com/wiremuxhq/wiremux/issues/307)) ([b321376](https://github.com/wiremuxhq/wiremux/commit/b321376174333df7ab9a24a0b33456ccbb04337d))
* decode Responses stream output_audio ([#315](https://github.com/wiremuxhq/wiremux/issues/315)) ([c9dde75](https://github.com/wiremuxhq/wiremux/commit/c9dde756ec35b4441f243bdf13bd0550e1c27067))
* decode source image blocks into ImageDelta ([#309](https://github.com/wiremuxhq/wiremux/issues/309)) ([5248cc0](https://github.com/wiremuxhq/wiremux/commit/5248cc04117472b6bb43ee294b56d8cdf6f97dff))
* fail streams that end on an error or a truncated frame ([#305](https://github.com/wiremuxhq/wiremux/issues/305)) ([987abc3](https://github.com/wiremuxhq/wiremux/commit/987abc328919334d5fe8ceff515dda2655fea78b))
* keep Chat audio and image on singular decode ([#319](https://github.com/wiremuxhq/wiremux/issues/319)) ([10e926f](https://github.com/wiremuxhq/wiremux/commit/10e926f47a50bf9c376b6959feff7a9bbb612a17))
* keep Converse stream audio bytes ([#314](https://github.com/wiremuxhq/wiremux/issues/314)) ([fa8f410](https://github.com/wiremuxhq/wiremux/commit/fa8f41032accae7139d816db06f9a61d4a4d3563))
* keep image text order on Responses and Converse ([#308](https://github.com/wiremuxhq/wiremux/issues/308)) ([e86f31c](https://github.com/wiremuxhq/wiremux/commit/e86f31cde54e17d35223ad1a138d4330bc85dd63))
* keep Responses text and non-data image URLs ([#317](https://github.com/wiremuxhq/wiremux/issues/317)) ([98d289d](https://github.com/wiremuxhq/wiremux/commit/98d289d2ef9ba8b29b3a264f48cc800bde14bd7e))
* refuse a singular decode that drops an audio transcript ([#318](https://github.com/wiremuxhq/wiremux/issues/318)) ([2fb6dfa](https://github.com/wiremuxhq/wiremux/commit/2fb6dfa8cd3dce54bebd6d50e34dd2b37351355e))

## [0.9.1](https://github.com/wiremuxhq/wiremux/compare/v0.9.0...v0.9.1) (2026-09-25)


### Bug Fixes

* keep Chat tool arguments that arrive as JSON ([#300](https://github.com/wiremuxhq/wiremux/issues/300)) ([e9784d3](https://github.com/wiremuxhq/wiremux/commit/e9784d35cf199529582ae220d858ba3ba59f175e)), closes [#299](https://github.com/wiremuxhq/wiremux/issues/299)
* keep Messages tool arguments before content_block_stop ([#298](https://github.com/wiremuxhq/wiremux/issues/298)) ([b46acf4](https://github.com/wiremuxhq/wiremux/commit/b46acf43613064ee237088716141c96c9fb8c6df))
* refuse Chat decode_stream_event that drops tool arguments ([#296](https://github.com/wiremuxhq/wiremux/issues/296)) ([1eb22c2](https://github.com/wiremuxhq/wiremux/commit/1eb22c249b1fe2da90c7a48ed2ee0aa840f929a7)), closes [#295](https://github.com/wiremuxhq/wiremux/issues/295)

## [0.9.0](https://github.com/wiremuxhq/wiremux/compare/v0.8.0...v0.9.0) (2026-09-25)


### Features

* dest-model crate-root encode and TransientKind is_reset ([#276](https://github.com/wiremuxhq/wiremux/issues/276)) ([a0c4d0b](https://github.com/wiremuxhq/wiremux/commit/a0c4d0b36b5d7b66ba97679607186781892a6909)), closes [#271](https://github.com/wiremuxhq/wiremux/issues/271) [#272](https://github.com/wiremuxhq/wiremux/issues/272) [#273](https://github.com/wiremuxhq/wiremux/issues/273) [#274](https://github.com/wiremuxhq/wiremux/issues/274)


### Bug Fixes

* Chat finish_reason tool_calls for a Responses function call ([#287](https://github.com/wiremuxhq/wiremux/issues/287)) ([a948bbf](https://github.com/wiremuxhq/wiremux/commit/a948bbf0d52537130c24aabd36bd589172f8c2d2))
* Chat stream finish_reason tool_calls after a custom tool ([#291](https://github.com/wiremuxhq/wiremux/issues/291)) ([4144a7a](https://github.com/wiremuxhq/wiremux/commit/4144a7af56747827587d46b682d7f4fe418e9819))
* Chat stream finish_reason tool_calls after a tool ([#290](https://github.com/wiremuxhq/wiremux/issues/290)) ([6da184e](https://github.com/wiremuxhq/wiremux/commit/6da184e9734df344b5972329a7e29a92d20c5472))
* do not run a cut-off tool call or steal a Gemini response ([#292](https://github.com/wiremuxhq/wiremux/issues/292)) ([64d45eb](https://github.com/wiremuxhq/wiremux/commit/64d45eb77a132550bfb9a64694c2df39b169ec18))
* drop profile debug text from overlay path tests ([#293](https://github.com/wiremuxhq/wiremux/issues/293)) ([74dded8](https://github.com/wiremuxhq/wiremux/commit/74dded888dc3a6e96db46800bc0ffd1ec1f4c94a))
* keep Gemini tool calls and unknown finish reasons ([#284](https://github.com/wiremuxhq/wiremux/issues/284)) ([ccb41da](https://github.com/wiremuxhq/wiremux/commit/ccb41da9140250f09e4acd258321a2e72a6ff859))
* **maps:** dest Responses STREAM metadata remaps dest Chat STREAM ([#278](https://github.com/wiremuxhq/wiremux/issues/278)) ([8ca7a72](https://github.com/wiremuxhq/wiremux/commit/8ca7a724fa3806b9993b1c07f798e1f3049e6e54))
* number parallel tool calls ([#285](https://github.com/wiremuxhq/wiremux/issues/285)) ([f8e8bf4](https://github.com/wiremuxhq/wiremux/commit/f8e8bf4548fa8f24f67ab33e38c72344875c9093))
* pair same-name Gemini function responses in order ([#286](https://github.com/wiremuxhq/wiremux/issues/286)) ([12f641c](https://github.com/wiremuxhq/wiremux/commit/12f641cdb66a4be354ea57ee3084dcb5733d13a0))
* surface Chat function calls that include arguments ([#289](https://github.com/wiremuxhq/wiremux/issues/289)) ([5e15135](https://github.com/wiremuxhq/wiremux/commit/5e151352311b0ec9b36f7512313880a29a7f5033))

## [0.8.0](https://github.com/wiremuxhq/wiremux/compare/v0.7.0...v0.8.0) (2026-09-20)


### Features

* expose TransientKind on ClientError ([#221](https://github.com/wiremuxhq/wiremux/issues/221)) ([861ae3f](https://github.com/wiremuxhq/wiremux/commit/861ae3f496dab333d0efdf13b238946d5259a9d3)), closes [#216](https://github.com/wiremuxhq/wiremux/issues/216)
* ship openai-codex API-key profile ([#219](https://github.com/wiremuxhq/wiremux/issues/219)) ([728e6c2](https://github.com/wiremuxhq/wiremux/commit/728e6c28d29d240bc9d2f1dddb62f620eec2a0e0))


### Bug Fixes

* **cli:** accept positional auth profile and drop login-flow hint ([#222](https://github.com/wiremuxhq/wiremux/issues/222)) ([e6530dc](https://github.com/wiremuxhq/wiremux/commit/e6530dc4139a63c21404884ff0c11654947b0ba1))
* dest Chat audio remaps dest Messages transcript and dest Gemini ([fbb6062](https://github.com/wiremuxhq/wiremux/commit/fbb6062799af72ae45c5abafabfd0be05da94bea))
* dest Chat audio remaps dest Messages transcript and dest Gemini inlineData ([#230](https://github.com/wiremuxhq/wiremux/issues/230)) ([fbb6062](https://github.com/wiremuxhq/wiremux/commit/fbb6062799af72ae45c5abafabfd0be05da94bea))
* dest Chat complete created remaps dest Responses created_at ([#242](https://github.com/wiremuxhq/wiremux/issues/242)) ([11e9628](https://github.com/wiremuxhq/wiremux/commit/11e9628c6053cd6cd881775f6ff29cee4988bbe9))
* dest Chat complete function_call remaps dest Messages and dest Gemini ([#234](https://github.com/wiremuxhq/wiremux/issues/234)) ([d44110f](https://github.com/wiremuxhq/wiremux/commit/d44110f893e90b800e522d4feebda8bd2d0c15c5))
* dest Chat complete service_tier remaps dest Responses complete service_tier ([#247](https://github.com/wiremuxhq/wiremux/issues/247)) ([4441195](https://github.com/wiremuxhq/wiremux/commit/44411955faa7bf1a739ee9eea039e7f17e60f9c0))
* dest Chat remaining dest Converse service_tier cache-read and dest Responses cancelled dest Chat stop ([#249](https://github.com/wiremuxhq/wiremux/issues/249)) ([98906e1](https://github.com/wiremuxhq/wiremux/commit/98906e1795d309e939c2821d6faebdba5cb550db))
* dest Chat remapped complete and stream keep dest model ([#223](https://github.com/wiremuxhq/wiremux/issues/223)) ([50ac3b7](https://github.com/wiremuxhq/wiremux/commit/50ac3b7ff1f91c7c7792ba8cfccd3dc6c6d14936))
* dest Chat service_tier and refusal remap dest Messages usage.service_tier and stop_details ([#259](https://github.com/wiremuxhq/wiremux/issues/259)) ([782bd03](https://github.com/wiremuxhq/wiremux/commit/782bd038fbb02c9b9ca1fe88e09829a97e349768))
* dest Chat service_tier audio and tool_calls id remap dest Gemini ([#263](https://github.com/wiremuxhq/wiremux/issues/263)) ([8fbcf80](https://github.com/wiremuxhq/wiremux/commit/8fbcf80336a02c2f56c9f4a16de96da611c3a6c9))
* dest Chat STREAM annotations remaps dest Messages and dest Gemini ([#237](https://github.com/wiremuxhq/wiremux/issues/237)) ([67468e6](https://github.com/wiremuxhq/wiremux/commit/67468e6765f55bdefe8ac015981323efbb462d1c))
* dest Chat STREAM audio remaps dest Gemini, dest Messages, and dest Responses ([#236](https://github.com/wiremuxhq/wiremux/issues/236)) ([c0ed4df](https://github.com/wiremuxhq/wiremux/commit/c0ed4df91f14e76c49bd71f0c1dda16a05ce3944))
* dest Chat STREAM created remaps dest Responses STREAM created_at ([#243](https://github.com/wiremuxhq/wiremux/issues/243)) ([e2aab71](https://github.com/wiremuxhq/wiremux/commit/e2aab71d72f309793951c151774639082d410b86))
* dest Chat STREAM function_call remaps dest Messages and dest Gemini ([#235](https://github.com/wiremuxhq/wiremux/issues/235)) ([ff07b74](https://github.com/wiremuxhq/wiremux/commit/ff07b74214c4c421809d52fd649a90369b97ba99))
* dest Chat STREAM logprobs remaps dest Gemini and dest Responses ([#232](https://github.com/wiremuxhq/wiremux/issues/232)) ([16e21a8](https://github.com/wiremuxhq/wiremux/commit/16e21a8adf845e05746a1d4346487e3e07805126))
* dest Chat STREAM logprobs.refusal remaps dest Gemini ([#233](https://github.com/wiremuxhq/wiremux/issues/233)) ([8e91e6a](https://github.com/wiremuxhq/wiremux/commit/8e91e6a62f8c7946fd3edac0b7acd55050275c49))
* dest Chat STREAM moderation remaps dest Responses STREAM nested moderation ([#264](https://github.com/wiremuxhq/wiremux/issues/264)) ([0ade2f9](https://github.com/wiremuxhq/wiremux/commit/0ade2f9871f0e630ff817a9fb9ac1a85fa557b68))
* dest Chat url_citation remapped dest Messages, Gemini, and Converse ([e2c20d1](https://github.com/wiremuxhq/wiremux/commit/e2c20d13c40716d2452189d200aacb325e5a759b))
* dest Chat url_citation remaps dest Messages, Gemini, and Converse ([#227](https://github.com/wiremuxhq/wiremux/issues/227)) ([e2c20d1](https://github.com/wiremuxhq/wiremux/commit/e2c20d13c40716d2452189d200aacb325e5a759b))
* dest Converse complete reasoningContent and audio remap dest Chat ([#262](https://github.com/wiremuxhq/wiremux/issues/262)) ([ff35ea3](https://github.com/wiremuxhq/wiremux/commit/ff35ea34a13769ef0baa15d179267efffc7ffae9))
* dest Converse complete usage.totalTokens and citationsContent content remap dest Chat ([#261](https://github.com/wiremuxhq/wiremux/issues/261)) ([4050363](https://github.com/wiremuxhq/wiremux/commit/405036366e1e7a67e1c83ab43d0628931981dcf6))
* dest Gemini AUDIO promptTokensDetails remaps dest Chat audio_tokens ([#256](https://github.com/wiremuxhq/wiremux/issues/256)) ([1b833f0](https://github.com/wiremuxhq/wiremux/commit/1b833f060bedff8b59369a4d93a469439524ae26)), closes [#255](https://github.com/wiremuxhq/wiremux/issues/255)
* dest Gemini candidatesTokensDetails AUDIO remaps dest Chat completion audio_tokens ([#257](https://github.com/wiremuxhq/wiremux/issues/257)) ([575d628](https://github.com/wiremuxhq/wiremux/commit/575d6285fe9b387cdbd2bfdfce8d344b518423a7))
* dest Gemini complete responseId dest identity and usageMetadata.totalTokenCount ([#252](https://github.com/wiremuxhq/wiremux/issues/252)) ([56d67e0](https://github.com/wiremuxhq/wiremux/commit/56d67e0d5cf9baf788f1c0e4d8edf56b0c4b5dc3))
* dest Gemini complete usageMetadata.serviceTier remaps dest Chat complete service_tier ([#250](https://github.com/wiremuxhq/wiremux/issues/250)) ([057c2e9](https://github.com/wiremuxhq/wiremux/commit/057c2e956b62e3c75ba862926c08daa293ecd679))
* dest Gemini grounding and dest Converse citation remap dest Chat ([8206f7f](https://github.com/wiremuxhq/wiremux/commit/8206f7fb959047d4892197dde9ebd884ecbf8f5b))
* dest Gemini grounding and dest Converse citation remap dest Chat ([#229](https://github.com/wiremuxhq/wiremux/issues/229)) ([8206f7f](https://github.com/wiremuxhq/wiremux/commit/8206f7fb959047d4892197dde9ebd884ecbf8f5b))
* dest Gemini STREAM citation spans remaps dest Chat STREAM start_index ([#245](https://github.com/wiremuxhq/wiremux/issues/245)) ([12574cd](https://github.com/wiremuxhq/wiremux/commit/12574cd09795c0094b5267370bcfeec1fef615ca))
* dest Gemini STREAM citationMetadata remaps dest Chat url_citation ([#238](https://github.com/wiremuxhq/wiremux/issues/238)) ([cecb102](https://github.com/wiremuxhq/wiremux/commit/cecb102c337987d64860fb96d1daba4f62e301c9))
* dest Gemini STREAM groundingAttributions remaps dest Chat STREAM annotations ([#244](https://github.com/wiremuxhq/wiremux/issues/244)) ([0dcbc80](https://github.com/wiremuxhq/wiremux/commit/0dcbc807492fc44097f98ed8a654a8d64b0e0b74))
* dest Gemini STREAM groundingChunks image/retrieved/maps remaps dest Chat STREAM url_citation ([#246](https://github.com/wiremuxhq/wiremux/issues/246)) ([e94f109](https://github.com/wiremuxhq/wiremux/commit/e94f1091dff5b15ad70de2365e811f4637d60e10))
* dest Gemini STREAM tokenCount remaps dest Chat usage ([#241](https://github.com/wiremuxhq/wiremux/issues/241)) ([6f93d09](https://github.com/wiremuxhq/wiremux/commit/6f93d09456b3851330c0818a213b13755e96f660))
* dest Gemini urlContextMetadata and citationSources remap dest Chat url_citation ([#266](https://github.com/wiremuxhq/wiremux/issues/266)) ([f28a936](https://github.com/wiremuxhq/wiremux/commit/f28a9367742c2efe4466381d8afd2f661daf3f7f))
* dest ingest catalog miss beats shipped skip ([#224](https://github.com/wiremuxhq/wiremux/issues/224)) ([020e3bb](https://github.com/wiremuxhq/wiremux/commit/020e3bb8dff806beee8915014a8b35702b7415d8))
* dest Messages and dest Converse skip empty audio-byte frames ([efa10f4](https://github.com/wiremuxhq/wiremux/commit/efa10f471cd6db739e9635978e3b8a63c8ce81a0))
* dest Messages and dest Converse skip empty audio-byte frames ([#231](https://github.com/wiremuxhq/wiremux/issues/231)) ([efa10f4](https://github.com/wiremuxhq/wiremux/commit/efa10f471cd6db739e9635978e3b8a63c8ce81a0))
* dest Messages citations_delta remaps dest Chat url_citation ([4e284bb](https://github.com/wiremuxhq/wiremux/commit/4e284bb00f0689a1cd25d57f4941d5fb6e551629))
* dest Messages citations_delta remaps dest Chat url_citation ([#228](https://github.com/wiremuxhq/wiremux/issues/228)) ([4e284bb](https://github.com/wiremuxhq/wiremux/commit/4e284bb00f0689a1cd25d57f4941d5fb6e551629))
* dest Messages complete usage.service_tier remaps dest Chat service_tier and stop_sequence dest Chat stop ([#251](https://github.com/wiremuxhq/wiremux/issues/251)) ([bdc8d97](https://github.com/wiremuxhq/wiremux/commit/bdc8d9739bcb4b118e1201fbd48369518b33de8d))
* dest Messages STREAM stop_details.explanation remaps dest Chat STREAM delta.refusal ([#260](https://github.com/wiremuxhq/wiremux/issues/260)) ([ca122de](https://github.com/wiremuxhq/wiremux/commit/ca122de3b2dfb0377e442c4f98269d92b80fc4ea))
* dest Responses complete failed remaps dest Chat finish_reason stop ([#248](https://github.com/wiremuxhq/wiremux/issues/248)) ([f84339c](https://github.com/wiremuxhq/wiremux/commit/f84339c2cb4166c7cde2bfb57a2a65ddc2797e7b))
* dest Responses complete metadata remaps dest Chat complete metadata ([#253](https://github.com/wiremuxhq/wiremux/issues/253)) ([0946d70](https://github.com/wiremuxhq/wiremux/commit/0946d706cc31b337866823f5f5a524d3120f9f26))
* dest Responses complete moderation remaps dest Chat complete moderation ([#254](https://github.com/wiremuxhq/wiremux/issues/254)) ([2a21b57](https://github.com/wiremuxhq/wiremux/commit/2a21b57e44142b6a5ee348fbdf9dfe0f78dd2a79))
* dest Responses complete output_audio remaps dest Chat message.audio ([#265](https://github.com/wiremuxhq/wiremux/issues/265)) ([b4939c6](https://github.com/wiremuxhq/wiremux/commit/b4939c627dab50afe1e94f9d0925d8c39b11ce99))
* dest Responses complete output_text logprobs remaps dest Chat ([#239](https://github.com/wiremuxhq/wiremux/issues/239)) ([1a7b114](https://github.com/wiremuxhq/wiremux/commit/1a7b1146910d7bb41bea95cf145f1e0f01980c1e))
* dest Responses STREAM created_at, service_tier, and moderation remap dest Chat STREAM ([#258](https://github.com/wiremuxhq/wiremux/issues/258)) ([3aececb](https://github.com/wiremuxhq/wiremux/commit/3aececbefd15ec1a559bf8711ca0ba624c0ddec9))
* dest Responses STREAM output_text.done keeps logprobs ([#240](https://github.com/wiremuxhq/wiremux/issues/240)) ([018bd89](https://github.com/wiremuxhq/wiremux/commit/018bd8937c2a7c8a3e25d114bfd3a9feb8768dff))
* dest Responses stream remaps Chat annotations, audio, and custom tools ([#226](https://github.com/wiremuxhq/wiremux/issues/226)) ([5853916](https://github.com/wiremuxhq/wiremux/commit/5853916a2fd63ced6cd8109a18092b89a7ac1287))
* dest Responses stream remaps Chat refusal, filter, and usage ([#225](https://github.com/wiremuxhq/wiremux/issues/225)) ([95f6e88](https://github.com/wiremuxhq/wiremux/commit/95f6e88461ea426934e9778b22b4cb2c9738e694))

## [0.7.0](https://github.com/wiremuxhq/wiremux/compare/v0.6.0...v0.7.0) (2026-09-18)


### Features

* dest-encode Converse remaps as Event Stream ([#178](https://github.com/wiremuxhq/wiremux/issues/178)) ([199af11](https://github.com/wiremuxhq/wiremux/commit/199af11a5159a05fe7388fb69109f6b93e5b09a2))


### Bug Fixes

* **ci:** run nightly cargo-fuzz on the GNU target ([#208](https://github.com/wiremuxhq/wiremux/issues/208)) ([8f708ac](https://github.com/wiremuxhq/wiremux/commit/8f708ac2b94a33509d3a73b510e5fa11e847a34b))
* dest Chat logit_bias, prediction, and web_search_options reach Chat ([#214](https://github.com/wiremuxhq/wiremux/issues/214)) ([6675c37](https://github.com/wiremuxhq/wiremux/commit/6675c37e4e389bb105722cf004a34aff30ec9000))
* dest Chat prompt_cache_options, top_logprobs, moderation, and stream obfuscation reach Chat ([#209](https://github.com/wiremuxhq/wiremux/issues/209)) ([d3cb375](https://github.com/wiremuxhq/wiremux/commit/d3cb375adccd2bf295e084cd173e0a25c068a8a9))
* dest Chat user reaches Chat ([#194](https://github.com/wiremuxhq/wiremux/issues/194)) ([56a879b](https://github.com/wiremuxhq/wiremux/commit/56a879bae9411a76d841009a0e3ed4b99ae53549))
* dest Converse image and audio reach Chat ([#202](https://github.com/wiremuxhq/wiremux/issues/202)) ([9dff5e5](https://github.com/wiremuxhq/wiremux/commit/9dff5e55a712762001b96913e235628a8c960831))
* dest Converse requestMetadata reaches Chat metadata ([#207](https://github.com/wiremuxhq/wiremux/issues/207)) ([4420a49](https://github.com/wiremuxhq/wiremux/commit/4420a493e32acbf943d331fbecd4d7ed0b6bca50))
* dest Converse stopReason, block index, and wrap ([#179](https://github.com/wiremuxhq/wiremux/issues/179)) ([32dfd47](https://github.com/wiremuxhq/wiremux/commit/32dfd47afadf47fa6cb0d776f8b6da077a6d6575))
* dest Gemini frequencyPenalty, presencePenalty, seed, and candidateCount reach Chat ([#210](https://github.com/wiremuxhq/wiremux/issues/210)) ([cabe097](https://github.com/wiremuxhq/wiremux/commit/cabe0978a114c1f2937a41016476b9ea34ee3157))
* dest Gemini functionResponse reuses the functionCall id ([#189](https://github.com/wiremuxhq/wiremux/issues/189)) ([2e9b334](https://github.com/wiremuxhq/wiremux/commit/2e9b3342cee151f15138171017e6bbf5b99abe15))
* dest Gemini logprobs reaches Chat top_logprobs ([#211](https://github.com/wiremuxhq/wiremux/issues/211)) ([120e47d](https://github.com/wiremuxhq/wiremux/commit/120e47d03982e9af1308bacf83ecf2061e92956d))
* dest Gemini path model as modelVersion ([#185](https://github.com/wiremuxhq/wiremux/issues/185)) ([7d01db2](https://github.com/wiremuxhq/wiremux/commit/7d01db2c273eb12c3daf57712e4b8fbb1fa95edf))
* dest Gemini responseFormat and parametersJsonSchema reach Chat ([#204](https://github.com/wiremuxhq/wiremux/issues/204)) ([64c1592](https://github.com/wiremuxhq/wiremux/commit/64c1592c4e60710e59f5888438e4daf9cd1961a0))
* dest Gemini responseLogprobs reaches Chat logprobs ([#213](https://github.com/wiremuxhq/wiremux/issues/213)) ([837188d](https://github.com/wiremuxhq/wiremux/commit/837188d36db0ee7da574b3c7eaaebc44d59b8b76))
* dest Gemini responseModalities and speechConfig reach Chat ([#212](https://github.com/wiremuxhq/wiremux/issues/212)) ([7d88ec6](https://github.com/wiremuxhq/wiremux/commit/7d88ec673176361ff00ebb22249d752a489e43ab))
* dest Gemini responseSchema reaches Chat json_schema ([#190](https://github.com/wiremuxhq/wiremux/issues/190)) ([97ad67d](https://github.com/wiremuxhq/wiremux/commit/97ad67da2aa284a555622fc5204417889496b334))
* dest Gemini store and serviceTier reach Chat ([#201](https://github.com/wiremuxhq/wiremux/issues/201)) ([19c35f2](https://github.com/wiremuxhq/wiremux/commit/19c35f2add5019c775fc28f4d40d6c58bb14774a))
* dest Gemini stream path and Messages Vertex model ([#182](https://github.com/wiremuxhq/wiremux/issues/182)) ([7c848f1](https://github.com/wiremuxhq/wiremux/commit/7c848f173c005d0b3b6503c7b9d6c15e7580117b))
* dest json_object reaches Chat response_format ([#193](https://github.com/wiremuxhq/wiremux/issues/193)) ([d646a1f](https://github.com/wiremuxhq/wiremux/commit/d646a1f79c7554868180e93b4eec21d981aa5652))
* dest Messages metadata.user_id reaches Chat user ([#199](https://github.com/wiremuxhq/wiremux/issues/199)) ([9858d6d](https://github.com/wiremuxhq/wiremux/commit/9858d6dbe0e50bc6053e7b06e146717fe2fe3fed))
* dest Messages output_config reaches Chat ([#200](https://github.com/wiremuxhq/wiremux/issues/200)) ([54b7e24](https://github.com/wiremuxhq/wiremux/commit/54b7e24824ea38039f340b5e1ea37d6274d46f32))
* dest Messages path model in message_start and unary JSON ([#184](https://github.com/wiremuxhq/wiremux/issues/184)) ([6714f98](https://github.com/wiremuxhq/wiremux/commit/6714f9892bcee30f086d3a592c38ed4df3f7478c))
* dest Messages service_tier and disable_parallel reach Chat ([#203](https://github.com/wiremuxhq/wiremux/issues/203)) ([b3e879e](https://github.com/wiremuxhq/wiremux/commit/b3e879ec6eaed6858322447b3b0ea149e4ac83ef))
* dest Responses path model on created and unary JSON ([#186](https://github.com/wiremuxhq/wiremux/issues/186)) ([a52c9fc](https://github.com/wiremuxhq/wiremux/commit/a52c9fc06396b6a1ada73b3d418bf7d3b19eb279))
* dest Responses prompt_cache_retention reaches Chat ([#206](https://github.com/wiremuxhq/wiremux/issues/206)) ([79a4219](https://github.com/wiremuxhq/wiremux/commit/79a4219118ce2a71d958bcfe1c40f5a23ebfdedd))
* dest Responses stream output_item.done keeps assembled text ([#191](https://github.com/wiremuxhq/wiremux/issues/191)) ([23dbf9b](https://github.com/wiremuxhq/wiremux/commit/23dbf9b51cfc10392d5aca4375686f5eeb3b5966))
* dest Responses stream output_item.done keeps reasoning summary ([#192](https://github.com/wiremuxhq/wiremux/issues/192)) ([70714bf](https://github.com/wiremuxhq/wiremux/commit/70714bf00dceb0ce23faaa0ba8cd988ab69ab04f))
* dest Responses stream output_item.done keeps the function_call ([#188](https://github.com/wiremuxhq/wiremux/issues/188)) ([5809075](https://github.com/wiremuxhq/wiremux/commit/58090753d11bf9bef803b6ae88fe3eebafd818b7))
* dest Responses tool turn status is completed ([#187](https://github.com/wiremuxhq/wiremux/issues/187)) ([df5eb2c](https://github.com/wiremuxhq/wiremux/commit/df5eb2c980acdb6acf511b3d20dcece116288b61))
* dest Responses verbosity, safety_identifier, and metadata reach Chat ([#205](https://github.com/wiremuxhq/wiremux/issues/205)) ([34be7a2](https://github.com/wiremuxhq/wiremux/commit/34be7a2dd329e0b62e98550472b79980936e1c55))
* emit Gemini and Converse stream terminals ([#172](https://github.com/wiremuxhq/wiremux/issues/172)) ([a393123](https://github.com/wiremuxhq/wiremux/commit/a393123ac1811e72719916034073bedcd58a16b9))
* keep Event Stream Content-Type on Converse passthrough ([#174](https://github.com/wiremuxhq/wiremux/issues/174)) ([89c47de](https://github.com/wiremuxhq/wiremux/commit/89c47dee44af2d0d1323437806c8316755b97383))
* lift dest Chat Azure deployment model from the request path ([#183](https://github.com/wiremuxhq/wiremux/issues/183)) ([b5af145](https://github.com/wiremuxhq/wiremux/commit/b5af1451583f1933f2a674fa6c287cb51cab99e3))
* lift dest Gemini and Converse model from the request path ([#180](https://github.com/wiremuxhq/wiremux/issues/180)) ([8a61023](https://github.com/wiremuxhq/wiremux/commit/8a61023b04ae8cd9634ed2b77b4058a7d58a3766))
* one Converse messageStop and no SSE on Event Stream errors ([#176](https://github.com/wiremuxhq/wiremux/issues/176)) ([3d3ac47](https://github.com/wiremuxhq/wiremux/commit/3d3ac4711489bfb849abc2d6e2caa1938cd94a34))
* put timeout and connect kind on Transient transport messages ([#181](https://github.com/wiremuxhq/wiremux/issues/181)) ([4632fe8](https://github.com/wiremuxhq/wiremux/commit/4632fe87186584eaddfe2a0efd39838731f6768f))
* take AsRef&lt;str&gt; on IrRequest::new and cache TTL ([#175](https://github.com/wiremuxhq/wiremux/issues/175)) ([5a4e31e](https://github.com/wiremuxhq/wiremux/commit/5a4e31e6f4cd7434e6875dc81b000046eed5891b))

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
