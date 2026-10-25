# -*- coding: utf-8 -*-
"""產生 UxPlayRs 應用程式圖示（stuff/UxPlayRs.Gui.ico 與 stuff/UxPlayRs.Gui.png）。

UxPlayRs 是純 Rust 的 AirPlay 2 接收端引擎，搭配 Windows GUI（UxPlayRs.Gui.exe），
可將 iPhone / iPad / Mac 的畫面與聲音以無線方式鏡像輸出到這部電腦。
圖示構圖對應此用途：漸層圓角底板上的白色「顯示器」，加上底緣中央朝上的
投放三角（AirPlay 慣用符號），代表「把行動裝置的畫面投放到這部螢幕」。

用法（儲存庫根目錄）：python stuff/generate_uxplayrs_icon.py
輸出：多尺寸 .ico（256/128/64/48/32/24/16）與 1024px 預覽 .png。
"""
from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw

# 主畫布邊長：先以高解析度繪製再縮小取樣，讓各尺寸的邊緣保持平滑。
MASTER = 1024
# .ico 內含的尺寸（Windows 殼層依顯示環境挑選最接近的大小）。
ICO_SIZES = (256, 128, 64, 48, 32, 24, 16)

# 背景漸層：頂部藍到底部紫，呼應 AirPlay／Windows Fluent 的色系。
GRAD_TOP = (47, 125, 246)     # #2F7DF6
GRAD_BOTTOM = (124, 58, 237)  # #7C3AED
# 投放三角顏色：與白色螢幕形成對比，縮到 16px 仍可辨識。
TRIANGLE = (72, 76, 227)      # #484CE3


def build_master() -> Image.Image:
    """繪製 1024px 主圖示，回傳 RGBA 影像。"""
    img = Image.new("RGBA", (MASTER, MASTER), (0, 0, 0, 0))

    # 背景圓角方形：先做 1px 高的垂直漸層，放大後透過圓角遮罩貼上。
    gradient = Image.new("RGB", (1, MASTER))
    for y in range(MASTER):
        t = y / (MASTER - 1)
        gradient.putpixel(
            (0, y),
            tuple(round(a + (b - a) * t) for a, b in zip(GRAD_TOP, GRAD_BOTTOM)),
        )
    gradient = gradient.resize((MASTER, MASTER))

    mask = Image.new("L", (MASTER, MASTER), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, MASTER - 1, MASTER - 1), radius=round(MASTER * 0.22), fill=255
    )
    img.paste(gradient, (0, 0), mask)

    draw = ImageDraw.Draw(img)

    # 白色圓角「顯示器」：佔滿上方主體，小尺寸時輪廓仍清楚。
    screen = (
        round(MASTER * 0.20),
        round(MASTER * 0.20),
        round(MASTER * 0.80),
        round(MASTER * 0.66),
    )
    draw.rounded_rectangle(screen, radius=round(MASTER * 0.05), fill=(255, 255, 255, 255))

    # 投放三角：底邊略低於螢幕下緣、頂點伸入螢幕中央，構成 AirPlay 式「投放」符號。
    triangle = (
        (round(MASTER * 0.375), round(MASTER * 0.74)),
        (round(MASTER * 0.625), round(MASTER * 0.74)),
        (round(MASTER * 0.50), round(MASTER * 0.40)),
    )
    draw.polygon(triangle, fill=(*TRIANGLE, 255))

    return img


def save_outputs(master: Image.Image, out_dir: Path) -> None:
    """把主圖示輸出為多尺寸 .ico 與 1024px 預覽 .png。"""
    # 每個尺寸各自從主圖以 LANCZOS 縮小，避免外掛自行縮圖造成的品質差異。
    resized = [master.resize((size, size), Image.LANCZOS) for size in ICO_SIZES]
    ico_path = out_dir / "UxPlayRs.Gui.ico"
    resized[0].save(ico_path, format="ICO", append_images=resized[1:])
    master.save(out_dir / "UxPlayRs.Gui.png", format="PNG")


def print_preview(master: Image.Image, size: int = 32) -> None:
    """在終端機列印 32px ASCII 預覽，供無法顯示圖片時目視檢查輪廓。"""
    small = master.resize((size, size), Image.LANCZOS)
    chars = " .:-=+*#%@"  # 依亮度由暗到亮對應的字元表
    for y in range(size):
        row = []
        for x in range(size):
            r, g, b, a = small.getpixel((x, y))
            if a < 128:
                row.append(" ")
            else:
                lum = (r * 299 + g * 587 + b * 114) // 1000
                row.append(chars[min(lum * len(chars) // 256, len(chars) - 1)])
        print("".join(row).rstrip())


def main() -> None:
    out_dir = Path(__file__).resolve().parent
    master = build_master()
    save_outputs(master, out_dir)
    print(f"written: {out_dir / 'UxPlayRs.Gui.ico'}")
    print(f"written: {out_dir / 'UxPlayRs.Gui.png'}")
    print_preview(master)


if __name__ == "__main__":
    main()
