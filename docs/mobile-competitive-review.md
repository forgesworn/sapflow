# Mobile broadcaster competition, priorities and distribution

Reviewed on 21 October 2026. This is a product assessment from official public
pages, not a physical-device benchmark. Prices can vary by storefront and tax.
The first Larix interoperability review was not a full feature review; this
document adds Larix's platform, Premium, FAQ, provisioning and control pages,
plus Moblin and IRLPro.

The product remains a standalone native iOS/Android broadcaster backed by our
SDK. Reliable publishing, recording and origin playback precede RelaySwarm.
Use our own SRT and RTMP implementations. Every official build must always
allow free, unlimited, watermark-free V4V broadcasting.

## Current competitive position

| App | Published features and availability | Published charging model | Implication |
| --- | --- | --- | --- |
| Larix | Native iOS/Android contribution app with SRT/RTMPS, adaptive bitrate, recording, audio-only, pause/standby and wider studio protocols | Free for 30 minutes, then another 30 with a mandatory overlay, then stops. US base Premium is $9.99/month or $119/year; removes time limits and adds features. NDI tier is $19.99/month including Premium | Strong contribution baseline. Benchmark the unrestricted app for long runs |
| Moblin | Apple-platform app with SRT/RTMPS, SRTLA/RIST bonding, stabilisation, recording, scenes, platform chat and alerts; MIT-licensed repository | App Store says all features are free; optional cosmetic purchases. UK examples shown at £1.99 and £4.99 | A paid timer does not beat it on price. Its IRL feature breadth is a serious benchmark |
| IRLPro | Android app with RTMP/SRT, SRTLA bonding, adaptive bitrate, platform chat and web overlays; advertises a free bonding service | Free to install on Google Play; reviewed official pages do not publish a current monthly/annual subscription tariff | Benchmark actual behaviour. Do not infer a paid plan or unlimited hosted service from an install price |

Sources: [Larix features](https://softvelum.com/larix/),
[Larix Premium and US pricing](https://softvelum.com/larix/premium/),
[Moblin App Store listing](https://apps.apple.com/gb/app/moblin/id6466745933),
[Moblin feature documentation](https://github.com/eerimoq/moblin/blob/main/README.md),
[Moblin licence](https://github.com/eerimoq/moblin/blob/main/LICENSE),
[IRLPro official features](https://irlpro.app/) and
[IRLPro Google Play listing](https://play.google.com/store/apps/details?id=app.irlpro.android).

IRLPro should not be described as verified open source. Its
[published terms](https://irlpro.app/terms/) restrict copying, modification
and source extraction; this review found no official open-source licence.
Do not repeat reported stability/battery problems as established comparisons:
we have not reproduced them on matched devices and current versions.

Larix also sells [Tuner](https://softvelum.com/larix-tuner/price/) at a base
$10/month/device for activation and management. Separate app functionality,
operator tooling, bonding relay and cloud production costs in any comparison.
Their charging models are not interchangeable.

## Features to build and qualify

These are our priorities, not claims that any listed feature is implemented.
The engineering SRT pilot can remain small. The recommended general release
adds B3R and B4R in the [delivery plan](mobile-streaming-plan.md).

| Capability | Priority and reason |
| --- | --- |
| Hardware H.264 video, native AAC, stable timestamps, preview and single-encode recording | Pilot essentials. Deliver a complete show with a recoverable recording before widening formats |
| Our encrypted SRT caller, latency/bandwidth controls, adaptive bitrate, reconnection and useful statistics | Pilot essentials. Congestion and queue age must influence our policy; socket connectivity alone is insufficient |
| Our RTMP publisher with RTMPS and certificate validation | Required before general v1. Support ordinary destinations and networks with no usable UDP path; automatic transport failover is separate |
| Audio-only with qualified lock/background behaviour, input meters, supported gain/routing, stereo/mono and music processing | General v1. V4V creators include music and audio shows; a video-only product misses a central use case |
| Qualified external/USB audio | General v1 with an explicit accessory/device matrix. Useful event audio matters more than adding 4K video |
| Focus, exposure, white balance, zoom, front/rear switching, fixed portrait/landscape | General v1 where supported. Select a stable encoded format before going live |
| Pause, mute and pre-start standby | General v1. Deliberate replacement video/silence must protect both transmitted and recorded media |
| Saved profiles, safe QR/deep-link import and clear destination preparation | General v1. Fast setup with explicit review; no automatic publication or long-lived credentials in shareable links |
| Qualified 1080p30 | General v1 on capable devices, after sustained qualification; 720p30 remains the common baseline |
| Stabilisation and small native graphics | High-priority follow-on for moving/IRL creators. Measure crop, latency, GPU and power; match settings in comparisons |

Larix documents platform-specific camera/audio controls and recording in its
[iOS](https://softvelum.com/larix/ios/) and
[Android](https://softvelum.com/larix/android/) pages. Its
[ABR FAQ](https://softvelum.com/larix/faq/) is a useful behaviour reference,
not a reason to copy thresholds without our own feedback and queue evidence.
Its [Grove](https://softvelum.com/larix/grove/) format demonstrates provisioning
through URLs/QR codes. A QR code does not encrypt embedded credentials. Reuse
Foregather only where its invitation contract fits; the destination retains
publishing authority.

## Later features and conditions

- Bonding is a major competitive requirement for an IRL-focused edition.
  Moblin and IRLPro already advertise it. Qualify independent interface use,
  receiver aggregation, link loss, data cost and energy; fast Wi-Fi/cellular
  failover is not bonding. Do not claim full IRL superiority before this works.
- Twitch/YouTube/Kick chat and alerts are valuable for those creators. Keep
  integrations optional and permission-scoped; avoid coupling our protocol
  engine to a platform login. V4V is the first integrated creator workflow.
- Talkback/IFB, tally and producer control follow a demonstrated production
  need. Larix's [talkback](https://softvelum.com/larix/talkback/) and
  [Tuner](https://softvelum.com/larix-tuner/) document the operator use case.
  Reuse ForgeSworn authority/transport components where appropriate rather
  than introducing another unrestricted control channel.
- WHIP is a separate option for destinations that need interactive latency;
  it is not required to prove SRT-to-origin broadcasting.
- HEVC, 60 fps, HDR/4K, multicamera and compositing need supported-device,
  destination and power evidence. Source-level format support is insufficient.
- NDI, Zixi, RIST, RTSP, phone-side SRT listener/rendezvous, screen/drone capture,
  arbitrary HTML/GPS widgets and a full fleet-management service have no
  current v1 requirement. Reconsider a named workflow rather than matching
  every competitor checkbox.
- Keep one phone uplink by default and fan out at the origin where practical.
  Multiple independent phone destinations require their own bandwidth/power
  justification and are distinct from bonding.

RelaySwarm follows reliable origin playback under its existing gates. Link
and Shelter can later improve recording delivery and recovery. Neither is a
substitute for the broadcaster's SRT/RTMPS publishing protocol.

## Distribution and charging proposal

Confirmed direction:

- Open-source application and reusable SDK. Select and publish the app's
  licence before its first public release; preserve existing Sapflow licences.
- Free Android distribution through Zapstore, with unrestricted app features,
  unlimited streaming to compatible destinations and no Google Play runtime
  requirement. Optional hosted services remain separate.
- Always-free V4V broadcasting across Zapstore, direct, Google Play and Apple
  builds. No timer, watermark, subscription check or paid transport/quality
  unlock may obstruct its supported broadcasting profiles, recording or recovery.

The owner's store proposal is free download with 30 minutes for other
destinations, then monthly or annual payment. The allowance's reset rule,
expiry behaviour, subscription prices and treatment of an active show are
open decisions. Explain any limit before Start; never surprise a creator
mid-show. A paid entitlement check must never affect a free V4V show.

Our recommendation is to compare that timer with free basic streaming and
paid production tools, optional managed bonding/relay, backup/storage or team
services. Moblin already makes its features free, so premium value must be
demonstrable. If the timer remains, keep it confined to external destinations
in the official store editions. Do not degrade V4V quality, safety or recovery
to differentiate a subscription.

Open-source client code permits community builds with different entitlements;
do not base the business on making a local timer impossible to remove. Sell
maintained distribution, useful software and optional services. Keep service
costs and promises explicit; unlimited free app use does not imply unlimited
free third-party relay capacity or storage.

Use optional StoreKit/Play billing adapters for a paid store edition and keep
them out of the media SDK and Zapstore build. Verify receipt restoration,
cancellation and offline/error behaviour. The current
[Apple payment/subscription guidelines](https://developer.apple.com/app-store/review/guidelines/#in-app-purchase)
and [Google Play payments policy](https://support.google.com/googleplay/android-developer/answer/9858738)
generally require their billing mechanisms for in-app digital upgrades, with
regional/programme exceptions. Recheck the chosen storefront rules at submission;
do not assume a Zap payment or QR entitlement can replace store billing.

Publish signed route-specific artefacts, checksums, source revisions and an
upgrade/migration policy. Test installation without Google Play for the
Zapstore route and test every store's purchase and free-V4V behaviour separately.

## What counts as beating the competition

The [native performance contract](native-performance-contract.md) proposes
20% lower energy than Larix, with lower peak memory and faster recovery at
matched quality. These targets are unmeasured. Use current Moblin and IRLPro
as additional platform baselines and retain failed as well as successful runs.

Compete on sustained delivery, energy/heat, recovery, audio quality, recording
integrity, setup, privacy, open distribution and V4V workflow. Each needs its
own evidence. A performance win does not establish wider feature coverage;
free V4V does not establish a price advantage over an already-free app.
Ship and describe measured strengths while the remaining goals stay open.
