(function () {
  if (typeof hljs === "undefined") {
    return;
  }

  hljs.registerLanguage("mollie", function (hljs) {
    const interpolation = {
      className: "subst",
      begin: /\$\{/,
      end: /\}/,
      keywords: { literal: "true false" },
    };

    return {
      name: "Mollie",
      keywords: {
        keyword:
          "as break const continue else enum extern for from func get if impl import in is let loop match module mut postfix pub return self Self set struct super trait value view while",
        literal: "true false None Some Ok Err",
        type: "i8 i16 i32 i64 isize u8 u16 u32 u64 usize f32 f64 bool string",
      },
      contains: [
        hljs.C_LINE_COMMENT_MODE,
        hljs.C_BLOCK_COMMENT_MODE,
        {
          className: "string",
          begin: /"/,
          end: /"/,
          contains: [{ begin: /\\[\s\S]/ }, interpolation],
        },
        { className: "symbol", begin: /'[A-Za-z_]\w*/ },
        { className: "number", begin: /\b\d[\d_]*(\.\d[\d_]*)?([a-z][a-z0-9]*)?\b/, relevance: 0 },
        {
          className: "title",
          begin: /\b[a-z_]\w*(?=\s*\()/,
          relevance: 0,
        },
        { className: "type", begin: /\b[A-Z]\w*/, relevance: 0 },
      ],
    };
  });

  // mdBook highlights code before additional scripts run, so blocks of Mollie
  // were left as plain text and are highlighted again here.
  document.querySelectorAll("code.language-mollie").forEach(function (block) {
    block.textContent = block.textContent;
    delete block.dataset.highlighted;

    if (hljs.highlightElement) {
      hljs.highlightElement(block);
    } else {
      hljs.highlightBlock(block);
    }
  });
})();
