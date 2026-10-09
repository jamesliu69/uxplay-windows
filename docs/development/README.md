# 開發與問題紀錄索引

這個目錄保存開發決策、問題根因、修正與驗證結果。查詢問題時，先看下表，再依紀錄中的問題編號、程式碼連結與命令繼續追查。

| 日期 | 紀錄 | 查詢關鍵字 |
|---|---|---|
| 2026-10-09 | [UxPlayRs 重寫、播放修正與安裝打包完整紀錄](2026-10-09-uxplay-rs-development-history.md) | C#、Rust、播放尺寸、停格、lag、無聲、AAC-ELD、色塊、重影、OpenH264、FFmpeg、MSI、WindowsBuild、9600、26300、1925 |

相關文件：

- [重寫版入口與目錄規則](../../rewrite/README.md)
- [Rust 引擎與播放行為](../../rewrite/engine/README.md)
- [WPF 控制介面](../../rewrite/gui/README.md)
- [建置、FFmpeg 隨附與 MSI 打包](../../rewrite/packaging/README.md)
- [原始播放改善計畫與各階段測試紀錄](../superpowers/plans/2026-10-09-mirroring-playback.md)

## 查詢方式

在儲存庫根目錄使用 PowerShell。例如搜尋停格、解碼器或安裝版本問題：

```powershell
rg -n '停格|B-frame|OpenH264|FFmpeg' docs/development
rg -n 'MSI|WindowsBuild|9600|1925' docs/development
```

後續每次修正，新增日期紀錄或在同一事件紀錄中追加更新，並更新本索引。至少記下症狀、重現條件、證據、根因、修改位置、驗證結果與未完成項目。保留中間失敗與已撤回的結論，不把舊階段的「已通過」當成最新驗收。

私人影片、解密後封包、配對身分與金鑰不放進文件或 Git。這裡保存診斷結論、合成測試來源與不含敏感資料的證據摘要。
