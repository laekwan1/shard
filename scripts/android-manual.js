// Generates the Android install/usage manual as a Word document.
//
// Word rather than Markdown because that is what gets read on a phone and
// forwarded without a viewer. Run with: node scripts/android-manual.js

const fs = require("fs");
const path = require("path");
const {
  Document, Packer, Paragraph, TextRun, HeadingLevel,
  Table, TableRow, TableCell, WidthType, BorderStyle, AlignmentType,
} = require("docx");

const ACCENT = "B07820";
const MUTED = "666666";

const h1 = (text) => new Paragraph({ text, heading: HeadingLevel.HEADING_1, spacing: { before: 360, after: 160 } });
const h2 = (text) => new Paragraph({ text, heading: HeadingLevel.HEADING_2, spacing: { before: 280, after: 120 } });
const p = (text, opts = {}) =>
  new Paragraph({ spacing: { after: 120 }, children: [new TextRun({ text, ...opts })] });
const note = (text) =>
  new Paragraph({ spacing: { after: 120 }, children: [new TextRun({ text, italics: true, color: MUTED, size: 20 })] });
const bullet = (text) => new Paragraph({ text, bullet: { level: 0 }, spacing: { after: 80 } });
const step = (text) => new Paragraph({ text, numbering: { reference: "steps", level: 0 }, spacing: { after: 100 } });
const code = (text) =>
  new Paragraph({
    spacing: { after: 120 },
    shading: { fill: "F2F0EC" },
    children: [new TextRun({ text, font: "Consolas", size: 19 })],
  });

const cell = (text, { bold = false, width = 33 } = {}) =>
  new TableCell({
    width: { size: width, type: WidthType.PERCENTAGE },
    margins: { top: 80, bottom: 80, left: 120, right: 120 },
    children: [new Paragraph({ children: [new TextRun({ text, bold, size: 20 })] })],
  });

const table = (header, rows) =>
  new Table({
    width: { size: 100, type: WidthType.PERCENTAGE },
    borders: {
      top: { style: BorderStyle.SINGLE, size: 1, color: "DDDDDD" },
      bottom: { style: BorderStyle.SINGLE, size: 1, color: "DDDDDD" },
      left: { style: BorderStyle.NONE }, right: { style: BorderStyle.NONE },
      insideHorizontal: { style: BorderStyle.SINGLE, size: 1, color: "EEEEEE" },
      insideVertical: { style: BorderStyle.NONE },
    },
    rows: [
      new TableRow({ children: header.map((t) => cell(t, { bold: true })) }),
      ...rows.map((r) => new TableRow({ children: r.map((t) => cell(t)) })),
    ],
  });

const doc = new Document({
  numbering: {
    config: [{
      reference: "steps",
      levels: [{ level: 0, format: "decimal", text: "%1.", alignment: AlignmentType.START }],
    }],
  },
  styles: {
    default: {
      document: { run: { font: "맑은 고딕", size: 21 }, paragraph: { spacing: { line: 300 } } },
      heading1: { run: { font: "맑은 고딕", size: 30, bold: true, color: ACCENT } },
      heading2: { run: { font: "맑은 고딕", size: 24, bold: true, color: "333333" } },
    },
  },
  sections: [{
    children: [
      new Paragraph({
        spacing: { after: 80 },
        children: [new TextRun({ text: "Shard · Veil 안드로이드 앱", bold: true, size: 40, color: ACCENT })],
      }),
      note("설치 · 사용 · 영상 저장 안내서"),

      h1("1. 두 앱은 무엇이 다른가"),
      p("둘 다 앱 안에 브라우저가 들어 있습니다. 그 브라우저로 사이트에 들어가면 됩니다. 크롬을 쓰지 않습니다. 차이는 브라우저 밑에서 무엇이 도는지입니다."),
      table(
        ["", "Shard", "Veil"],
        [
          ["하는 일", "차단 탐지를 흐트러뜨림", "서버를 거쳐 나감"],
          ["서버 필요", "없음", "있음 (오라클 등)"],
          ["속도", "직결과 동일", "서버 성능만큼"],
          ["내 IP", "그대로 노출", "서버 IP로 바뀜"],
          ["ISP가 보는 것", "어디에 접속하는지 보임", "서버 주소 하나만"],
          ["언제 쓰나", "그냥 막힌 사이트를 볼 때", "추적을 피하고 싶을 때"],
        ],
      ),
      note("평소에는 Shard로 충분합니다. Shard는 추가 경유지가 없어 속도 손해가 없습니다. 측정 결과 엔진을 거쳐도 8.51 MB/s로, 직결 8.46 MB/s와 차이가 없었습니다."),

      h1("2. 설치"),
      p("두 앱 모두 스토어에 올리지 않고 파일로 직접 설치합니다."),
      step("Shard.apk 와 Veil.apk 를 폰으로 옮깁니다. (USB 케이블, 카카오톡 나에게 보내기, 구글 드라이브 등 무엇이든 됩니다.)"),
      step("폰의 파일 앱에서 APK를 누릅니다."),
      step("\"이 출처의 앱 설치 허용\" 을 묻거든 켜 줍니다. 스토어를 거치지 않은 앱이라 한 번은 물어봅니다."),
      step("설치를 누릅니다. Play 프로텍트 경고가 나오면 \"무시하고 설치\" 를 선택합니다."),
      note("VPN 권한을 묻지 않습니다. 브라우저가 앱 안에 있어서, 이 앱에서 보는 것만 영향을 받고 다른 앱은 건드리지 않기 때문입니다."),

      h1("3. Shard 사용법"),
      p("열면 이미 켜져 있습니다. 주소창에 사이트를 입력하면 됩니다."),
      bullet("왼쪽 위 버튼: 켜짐 / 꺼짐. 끄면 그냥 일반 브라우저가 됩니다."),
      bullet("아래 줄: 지금까지 연결 수, 우회한 수, 그대로 통과시킨 수, 주고받은 양."),
      bullet("뒤로가기: 웹 페이지 뒤로. 더 갈 곳이 없으면 앱이 닫힙니다."),
      p("설정은 PC의 Shard 와 같은 파일 형식을 씁니다. 별도로 만질 것은 없습니다."),

      h1("4. Veil 사용법"),
      p("Veil 은 나갈 서버가 필요합니다. PC 의 Veil 에서 서버를 \"내보내기\" 하면 나오는 링크를 씁니다."),
      step("PC 에서 Veil 을 열고 서버 프로필의 [내보내기] 를 누릅니다."),
      step("link.txt 안의 vless:// 로 시작하는 한 줄을 복사합니다."),
      step("폰에서 Veil 을 열면 서버 링크를 묻습니다. 붙여넣고 [저장] 을 누릅니다."),
      step("잠시 뒤 아래 줄에 \"○○ 를 통해 연결됨\" 이 뜨면 됩니다."),
      p("서버를 바꾸려면 위쪽 [서버] 버튼을 다시 누르면 됩니다."),
      note("은행 · 증권 · 정부(go.kr) 사이트는 자동으로 서버를 거치지 않고 그대로 나갑니다. 해외 IP 로 접속하면 거절당하거나 계정이 잠기기 때문입니다. 이 목록은 앱에 이미 들어 있습니다."),

      h1("5. 영상 내려받기"),
      p("두 앱 모두 같은 방식입니다."),
      step("앱 안의 브라우저로 영상 페이지에 들어갑니다."),
      step("영상을 재생합니다. 재생이 시작되면 오른쪽 위 [받기] 옆에 숫자가 붙습니다."),
      step("[받기] 를 누르면 이 페이지에서 발견된 영상 목록이 나옵니다."),
      step("받을 것을 고르면 내려받기가 시작되고, 아래 줄에 진행률이 표시됩니다."),
      p("저장 위치: 갤러리 → 동영상 → Shard 폴더."),
      note("왜 앱 안의 브라우저여야 하는가: 페이지는 \"여기 영상이 있습니다\" 라고 알려주지 않고 그냥 가져갑니다. 그 요청을 옆에서 지켜봐야 받을 수 있는데, 크롬 밖에서는 그 요청이 보이지 않습니다."),
      p("끊긴 조각으로 전송되는 영상(HLS · m3u8)은 조각을 모두 받아 하나로 이어 붙입니다. 화질이 여러 개면 가장 높은 것을 고릅니다. 이런 영상은 .ts 파일로 저장되며, 대부분의 재생기가 그대로 재생합니다."),

      h1("6. 잘 안 될 때"),
      table(
        ["증상", "확인할 것"],
        [
          ["사이트가 안 뜬다", "왼쪽 위가 \"켜짐\" 인지 확인. 꺼져 있으면 우회하지 않습니다."],
          ["Veil 이 연결 안 됨", "[서버] 를 눌러 링크를 다시 붙여넣기. PC 의 Veil 에서 그 서버가 되는지 먼저 확인."],
          ["[받기] 에 아무것도 없다", "영상을 실제로 재생해야 목록에 잡힙니다. 재생 전에는 비어 있습니다."],
          ["내려받기가 실패한다", "엔진을 켠 채로 다시 시도. 내려받기도 같은 경로로 나가야 차단되지 않습니다."],
          ["앱이 설치가 안 된다", "\"출처를 알 수 없는 앱\" 허용이 필요합니다. Veil 은 arm64 폰 전용입니다."],
        ],
      ),

      h1("7. 알아둘 점"),
      bullet("Shard 는 우회만 합니다. 익명성은 없습니다 — 사이트는 여전히 내 IP 를 봅니다."),
      bullet("Veil 은 서버를 믿는 구조입니다. 내가 만든 오라클 서버라면 그 서버는 내 것입니다."),
      bullet("앱을 닫으면 엔진도 함께 멈춥니다. 백그라운드에 남지 않습니다."),
      bullet("두 앱은 서로 독립적입니다. 하나만 깔아도 되고, 둘 다 깔아 상황에 따라 골라 써도 됩니다."),
    ],
  }],
});

const out = path.join(__dirname, "..", "docs", "안드로이드-앱-사용설명서.docx");
Packer.toBuffer(doc).then((buffer) => {
  fs.writeFileSync(out, buffer);
  console.log("wrote " + out);
});
