import test from 'node:test';
import assert from 'node:assert/strict';
import { validatePdfBytes } from './native-live-pdf.mjs';

function validPdf({
  streamText = '',
  pageDictionary = '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 960 540] >>',
  pageTreeDictionary = '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
  extraObjects = '',
} = {}) {
  const lines = [
    '%PDF-1.7',
    '1 0 obj',
    '<< /Type /Catalog /Pages 2 0 R >>',
    'endobj',
    '2 0 obj',
    pageTreeDictionary,
    'endobj',
    '3 0 obj',
    pageDictionary,
    'endobj',
    '4 0 obj',
    '<< /Length ' + streamText.length + ' >>',
    'stream',
    streamText,
    'endstream',
    'endobj',
  ];
  const suffix = [
    'xref',
    '0 5',
    'trailer',
    '<< /Root 1 0 R >>',
    'startxref',
    '9',
    '%%EOF',
  ];
  return Buffer.from(lines.join('\n') + '\n' + extraObjects + suffix.join('\n') + '\n', 'latin1');
}

test('PDF 验收按间接对象确认单页和每页 MediaBox 并记录实际摘要', () => {
  const result = validatePdfBytes(validPdf({ streamText: '/Type /Page fake stream marker' }), {
    expectedPageCount: 1,
    expectedPageWidthPoints: 960,
    expectedPageHeightPoints: 540,
  });
  assert.equal(result.pageCount, 1);
  assert.deepEqual(result.pageSizes, [{ widthPoints: 960, heightPoints: 540 }]);
  assert.equal(result.sha256.length, 64);
  assert.ok(result.sizeBytes > 0);
});

test('PDF 验收拒绝改扩展名文本和错误页数', () => {
  assert.throws(() => validatePdfBytes(Buffer.from('not a pdf'), { expectedPageCount: 1 }), /header/);
  assert.throws(() => validatePdfBytes(validPdf(), { expectedPageCount: 2 }), /页数不匹配/);
});

test('PDF 验收支持从 PageTree 继承 MediaBox 和通过间接对象读取 MediaBox', () => {
  const inherited = validatePdfBytes(validPdf({
    pageDictionary: '<< /Type /Page /Parent 2 0 R >>',
    pageTreeDictionary: '<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 960 540] >>',
  }), { expectedPageCount: 1, expectedPageWidthPoints: 960, expectedPageHeightPoints: 540 });
  assert.deepEqual(inherited.pageSizes, [{ widthPoints: 960, heightPoints: 540 }]);

  const indirect = validatePdfBytes(validPdf({
    pageDictionary: '<< /Type /Page /Parent 2 0 R /MediaBox 5 0 R >>',
    extraObjects: '5 0 obj\n[0 0 960 540]\nendobj\n',
  }), { expectedPageCount: 1, expectedPageWidthPoints: 960, expectedPageHeightPoints: 540 });
  assert.deepEqual(indirect.pageSizes, [{ widthPoints: 960, heightPoints: 540 }]);
});

test('PDF 验收拒绝 Letter、错误比例和缺失或无效的几何尺寸', () => {
  assert.throws(() => validatePdfBytes(validPdf({
    pageDictionary: '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>',
  }), { expectedPageCount: 1, expectedPageWidthPoints: 960, expectedPageHeightPoints: 540 }), /页面尺寸不匹配/);
  assert.throws(() => validatePdfBytes(validPdf({
    pageDictionary: '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 960 600] >>',
  }), { expectedPageCount: 1, expectedPageWidthPoints: 960, expectedPageHeightPoints: 540 }), /页面尺寸不匹配/);
  assert.throws(() => validatePdfBytes(validPdf({
    pageDictionary: '<< /Type /Page /Parent 2 0 R >>',
    pageTreeDictionary: '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
  }), { expectedPageCount: 1 }), /缺少有效 MediaBox/);
  assert.throws(() => validatePdfBytes(validPdf({
    pageDictionary: '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 0 540] >>',
  }), { expectedPageCount: 1 }), /缺少有效 MediaBox/);
});

test('PDF 页面尺寸只允许计划声明的 point 容差', () => {
  const withinTolerance = validatePdfBytes(validPdf({
    pageDictionary: '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 960.019 539.981] >>',
  }), {
    expectedPageCount: 1,
    expectedPageWidthPoints: 960,
    expectedPageHeightPoints: 540,
    pageSizeTolerancePoints: 0.02,
  });
  assert.deepEqual(withinTolerance.pageSizes, [{ widthPoints: 960.019, heightPoints: 539.981 }]);
  assert.throws(() => validatePdfBytes(validPdf({
    pageDictionary: '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 960.021 540] >>',
  }), {
    expectedPageCount: 1,
    expectedPageWidthPoints: 960,
    expectedPageHeightPoints: 540,
    pageSizeTolerancePoints: 0.02,
  }), /页面尺寸不匹配/);
});
