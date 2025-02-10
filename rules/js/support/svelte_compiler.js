require(process.cwd() + '/.pnp.cjs').setup();

const fs = require('fs');
const path = require('path')
const { compile, preprocess, parse } = require('svelte/compiler');
const preprocessor = require('svelte-preprocess');
const tailwind = require('tailwindcss');
const postcss = require('postcss')

async function run() {
  const inputFilename = process.argv[2];

  const source = await fs.promises.readFile(inputFilename, 'utf8');
  const { compilerOptions } = JSON.parse(
    process.argv[3],
  );

  const tailwindInputSource = source.replace(/<style>(.+?)<\/style>/s, '')

  const { code: intermediateCode, map: intermediateSourcemap } = await preprocess(
    source, [
      preprocessor({
        postcss: {
          plugins: [tailwind({
            content: [{ raw: tailwindInputSource, extension: 'html' }],
          })],
        }
      })
    ], { filename: inputFilename },
  )
  
  // Generate any used Tailwind utility classes
  const { css: tailwindUtilitiesCss } = await postcss(tailwind({
    content: [{ raw: tailwindInputSource, extension: 'html' }],
  })).process('@tailwind utilities;', { from: "tailwind_utilities.css", map: false })

  const { js: { code: js, map: sourcemap }, css: { code: css, map: cssSourcemap }, warnings } = await compile(intermediateCode, {
    ...compilerOptions,
    sourcemap: intermediateSourcemap
  })

  for(const warning of warnings) {
    console.error(warning.toString());
  }

  const sourceMapPath = inputFilename + ".js.map"
  let cssPath = null
  let tailwindUtilitiesCssPath = null
  if(css){
    cssPath = inputFilename + ".css"
    await fs.promises.writeFile(cssPath, css)
  }
  if(tailwindUtilitiesCss) {
    tailwindUtilitiesCssPath = inputFilename + ".tailwind.css"
    await fs.promises.writeFile(tailwindUtilitiesCssPath, tailwindUtilitiesCss)
  }
  let outputJs = js;
  if (cssPath) {
    outputJs += `\nimport ${JSON.stringify("./" + path.basename(cssPath))};`
  }
  if (tailwindUtilitiesCssPath) {
    outputJs += `\nimport ${JSON.stringify("./" + path.basename(tailwindUtilitiesCssPath))};`
  }
  outputJs += "\n//# sourceMappingURL=./" + path.basename(sourceMapPath);
  await fs.promises.writeFile(inputFilename + ".js", outputJs)
  await fs.promises.writeFile(sourceMapPath, JSON.stringify(sourcemap))
}

run();