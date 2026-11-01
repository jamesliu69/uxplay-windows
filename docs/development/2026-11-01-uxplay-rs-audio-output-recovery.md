# 2026-11-01 UxPlayRs 音訊輸出熱恢復與預設裝置跟隨

## 症狀

UxPlayRs.Gui（WPF）啟動引擎後鏡像正常但沒有聲音，或播放中把 Windows
預設輸出裝置切到耳機／喇叭後聲音沒有跟過去。兩種情況都只能 Stop →
Start 引擎才能恢復。

## 重現條件

1. 啟動當下沒有可用的預設輸出裝置（藍牙耳機尚未連上、HDMI 音訊還
   沒就緒、裝置被停用），之後裝置出現。
2. AirPlay 播放中，在 Windows 音效設定把「預設輸出裝置」切到另一個
   裝置。

## 證據與根因

修改前的 `audio_output.rs`：

- `AudioOutputRuntime::start()` 在「沒有預設裝置」「查不到格式」「開
  串流失敗」三種情況直接回 `disabled()`：handler 永久接到 1 Hz 的
  樣板 ring，之後所有 PCM 都被丟棄，沒有任何重試路徑。
- `OutputWorker` 只在 `needs_rebuild`（串流錯誤回呼）時重開串流，
  沒有偵測「使用者切換預設裝置」；播放中切換後，串流仍綁在舊裝置。
- `SystemAudioSession` 在建立時快取 `output_channels`，裝置热插入
  後聲道數不同的裝置無法正確映射（多聲道會把 frame 順序排錯）。

## 修改內容

全部在 [audio_output.rs](../../rewrite/engine/crates/airplayd/src/audio_output.rs)：

1. 開啟失敗不再永久停用：`OutputWorker` 每 500 ms 重試（失敗 log
   每 20 次節流一次），裝置出現後自動開串流並把 ring 重設為實際格式。
2. 以 `DeviceId`（取不到時退回裝置名）比對「串流所在裝置」與「目前
   預設裝置」，不同即把串流搬到新裝置；開啟前連續兩次查詢預設裝置
   一致才動作，避免切換瞬間的 race。
3. 聲道數與取樣格式以第一次成功開啟的裝置為準終身沿用（與
   shairplay 的協商一致）；取樣率改為跟隨 AirPlay session
   `audio_init` 宣布的 PCM 速率，串流以該速率重開、由 WASAPI shared
   mode 轉換到裝置 mix format。純速率對齊時 ring 只重算計時欄位
   （容量／prefill／transition），不清空待播 PCM。
4. session 的聲道映射改為每次收包讀 ring 現值，裝置熱插入後聲道數
   變動也能正確映射。
5. `AudioRing::reconfigure` 保留 session id：換裝置屬於同一條
   AirPlay 連線的延續，舊 session 判定不受格式重設影響。

## 驗證

- `cargo test --workspace`：airplayd 47 通過（含新增
  `refresh_reason_orders_rebuild_then_rate_then_device_change`、
  `align_sample_rate_follows_pcm_and_keeps_device_channels`、
  `retune_rate_resizes_timing_without_clearing_pending_samples`、
  `reconfigure_resizes_buffer_keeps_session_id_and_clears_backlog`、
  `audio_without_a_device_does_not_delay_video`）、shairplay
  130+50、integration 32、airplay-ipc 7，全部通過；FFmpeg 相關測試
  需 `UXPLAY_FFMPEG_PATH` 指向隨附 ffmpeg.exe。
- 真裝置測試 `real_audio_device_consumes_pcm_after_output_recovery`
  （`--ignored --nocapture`）在本機真實 WASAPI 裝置上手動執行通過：
  以新路徑開串流、ring reconfigure、注入 `DeviceNotAvailable` 後
  worker 重建並繼續消耗 PCM。
- `cargo clippy -p airplayd --all-targets` 無本次新增警告（僅既有
  測試 chunks_exact 樣式與套件 metadata 警告）。

## 未完成項目

- 實體 iPhone 端到端聽感驗收（無法自動化，需人工確認）：特別是
  藍牙耳機 late-attach 與播放中切換裝置兩個情境。
- 藍牙耳機 hands-free（HFP）設定檔只有 8/16 kHz 雙向通道，若被選為
  預設裝置，WASAPI 轉換品質受限；此為系統行為，未在引擎端處理。
- GUI 未提供輸出裝置選擇；目前一律跟隨系統預設裝置。
