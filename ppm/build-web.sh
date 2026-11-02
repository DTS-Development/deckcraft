#!/usr/bin/env bash
# PPM 포크 웹 빌드: 한글 글꼴 세트(NanumGothic, 나눔명조, 나눔고딕코딩, Black Han Sans, Pretendard)와 Office 호환 글꼴(Carlito, Caladea, Liberation, Inter)을 넣어 dist/web 을 만든다.
# 이 글꼴만 들어간다(업스트림 릴리스에 있던 다른 글꼴은 빠진다).
# 필요: Rust 1.95+ (기본 1.99.1), wasm32-unknown-unknown target, trunk 0.21+
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
export CRAFT_FONTS_DIR="$root/ppm/fonts-ko"
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-1.99.1}"
app=deckcraft-web
cd "$root/apps/$app"
# ppm-host: host mode only (no local open/save/print; opened outside PPM it shows a notice).
# 인자 없음: release(PR·배포용). --fast: 로컬 확인용(LTO 없음, wasm-opt 건너뜀, 크기 큼).
case "${1:-}" in
  "") trunk build --release --features ppm-host ;;
  --fast) trunk build --cargo-profile web-fast --features ppm-host ;;
  *) echo "usage: $0 [--fast]" >&2; exit 2 ;;
esac
# OFL-1.1 §2 / Apache-2.0 §4: the license travels with the embedded font.
mkdir -p "$root/dist/web/licenses"
fonts="$root/ppm/fonts-ko/fonts"
cp "$fonts/nanum-gothic/OFL.txt" "$root/dist/web/licenses/NanumGothic-OFL.txt"
cp "$fonts/carlito/OFL.txt" "$root/dist/web/licenses/Carlito-OFL.txt"
cp "$fonts/caladea/LICENSE.txt" "$root/dist/web/licenses/Caladea-Apache-2.0.txt"
cp "$fonts/liberation/OFL.txt" "$root/dist/web/licenses/Liberation-OFL.txt"
cp "$fonts/inter/OFL.txt" "$root/dist/web/licenses/Inter-OFL.txt"
cp "$fonts/nanum-myeongjo/OFL.txt" "$root/dist/web/licenses/NanumMyeongjo-OFL.txt"
cp "$fonts/nanum-gothic-coding/OFL.txt" "$root/dist/web/licenses/NanumGothicCoding-OFL.txt"
cp "$fonts/black-han-sans/OFL.txt" "$root/dist/web/licenses/BlackHanSans-OFL.txt"
cp "$fonts/pretendard/LICENSE" "$root/dist/web/licenses/Pretendard-OFL.txt"
# 관리자 화면이 읽는 명령 목록(표와 같은지는 ui-egui 테스트가 확인한다).
cp "$root/ppm/commands.json" "$root/dist/web/commands.json"
