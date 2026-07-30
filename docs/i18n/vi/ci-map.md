# Bản đồ CI Workflow

Trang này mô tả các GitHub Actions hiện có và có thể thực thi trong repository.
Những tên workflow trong tài liệu lịch sử không hoạt động nếu không có file
tương ứng trong `.github/workflows/`.

Xem luồng theo sự kiện tại
[`.github/workflows/main-branch-flow.md`](../../../.github/workflows/main-branch-flow.md).

## Nền tảng Workflow Có thể Thực thi

| Workflow | Kích hoạt | Kết quả chính |
| --- | --- | --- |
| `ci-run.yml` | push/PR/merge queue cho `main`, `dev`; thủ công | xác thực Rust và web |
| `docs-deploy.yml` | PR docs/site vào `main`; push `main`; thủ công | build Pages và deploy từ `main` |

## Hợp đồng CI Cốt lõi

`.github/workflows/ci-run.yml` thực hiện:

- kiểm tra định dạng các file Rust thay đổi bằng Rust 1.92.0;
- `cargo clippy --locked --all-targets -- -D clippy::correctness`;
- `cargo test --locked`;
- `npm ci`, `npm test` và `npm run build` trong `web/`.

Tên kiểm tra tổng hợp ổn định là `CI Required Gate`. Kiểm tra này thất bại nếu
bất kỳ job Rust lint, Rust test hoặc web nào thất bại.

Kiểm tra định dạng là gia tăng vì cây hiện tại còn drift rustfmt toàn repository.
File Rust mới hoặc đã sửa vẫn luôn được kiểm tra.

## Hợp đồng Docs Pages

`.github/workflows/docs-deploy.yml`:

- build `site/` bằng Node.js 22;
- suy ra Vite base path từ tên repository;
- xác nhận docs manifest đã được commit và cập nhật;
- chỉ build, không deploy trên pull request;
- chỉ upload và deploy `gh-pages/` từ `main`.

Nguồn GitHub Pages của repository phải là **GitHub Actions**. Xem
[runbook deploy docs](../../operations/docs-deploy-runbook.md).

## Tái hiện Cục bộ

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D clippy::correctness
cargo test --locked

cd web
npm ci
npm test
npm run build

cd ../site
npm ci
VITE_BASE_PATH=/llamafarm/ npm run build
git diff --exit-code -- src/generated/docs-manifest.json
```

## Triage Nhanh

1. Với `CI Required Gate`, mở `ci-run.yml` và xem job phụ thuộc đầu tiên thất bại.
2. Với `Build Docs Site`, build lại `site/` và kiểm tra docs manifest có thay đổi
   hay không.
3. Với Pages, xác nhận run ở `main`, build thành công và nguồn Pages là
   **GitHub Actions**.
4. Với lỗi asset, kiểm tra HTML trong `gh-pages/` và base path của URL.

## Quy tắc Bảo trì

- Giữ tên `CI Required Gate` ổn định hoặc cập nhật branch rules đồng thời.
- Giữ phiên bản Rust và Node rõ ràng, đồng bộ với công cụ cục bộ.
- Pin GitHub Actions vào revision bất biến.
- Commit docs manifest cùng thay đổi tài liệu nguồn.
- Cập nhật bản đồ này, các bản địa hóa và required-check mapping khi workflow
  thực thi thay đổi.
