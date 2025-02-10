require(process.cwd() + '/.pnp.cjs').setup();

const {
  compileScript,
  compileStyleAsync,
  compileTemplate,
  parse,
} = require('@vue/compiler-sfc');
const postcssPresetEnv = require('postcss-preset-env');
const fs = require('fs');
const crypto = require('crypto');
const path = require('path');
const child_process = require('child_process');

async function run() {
  const inputFilename = process.argv[2];

  const { ssr, isProd, runtimeModuleName, relaySchema } = JSON.parse(
    process.argv[3],
  );

  const source = await fs.promises.readFile(inputFilename, 'utf8');
  const { descriptor, errors } = parse(source, { filename: inputFilename });

  // TODO: double check that filename is consistent accross platforms
  const id = crypto
    .createHash('md5')
    .update(inputFilename)
    .digest()
    .toString('hex')
    .substring(0, 8);
  const dataId = 'data-v-' + id;

  const hasScoped = descriptor.styles.some((o) => o.scoped);

  let contents = '';
  let script;
  const scriptFilename = inputFilename + '.script.ts';
  if (descriptor.script || descriptor.scriptSetup) {
    contents += `import component from ${JSON.stringify(
      './' + path.basename(scriptFilename),
    )};`;
    script = compileScript(descriptor, { id });
    await fs.promises.writeFile(scriptFilename, script.content);

    if (descriptor.scriptSetup?.attrs.relay) {
      const relayConfigFilePath = '/tmp/relay.config.json';
      await fs.promises.writeFile(
        relayConfigFilePath,
        JSON.stringify({
          src: path.dirname(scriptFilename),
          language: 'typescript',
          schema: relaySchema,
        }),
      );

      const relayBinary = require('relay-compiler');
      const { print, parse } = require('graphql');
      const process = child_process.spawn(
        relayBinary,
        ['compiler', relayConfigFilePath, '--output=quiet-with-errors'],
        {
          stdio: 'inherit',
        },
      );
      const code = await new Promise((resolve) => process.on('close', resolve));
      if (code !== 0) {
        throw new Error('relay compile failed');
      }

      const imports = [];
      const transformedContents = script.content.replaceAll(
        /graphql`([\s\S]*?)`/gm,
        (match, query) => {
          const formatted = print(parse(query));
          const name = /(fragment|mutation|query) (\w+)/.exec(formatted)[2];
          const hash = crypto
            .createHash('md5')
            .update(formatted, 'utf8')
            .digest('hex');

          let id = `graphql__${hash}`;
          imports.push(
            `import ${id} from "./__generated__/${name}.graphql.ts";`,
          );
          return id;
        },
      );

      await fs.promises.writeFile(
        scriptFilename,
        imports.join('\n') + transformedContents,
      );
    }
  } else {
    contents += `import { defineComponent } from ${JSON.stringify(
      runtimeModuleName,
    )}; const component = defineComponent({ setup() {} });`;
  }
  contents += `component.name = ${JSON.stringify(
    path.basename(inputFilename, '.vue'),
  )};`;

  for (const index in descriptor.styles) {
    const styleFilename = inputFilename + `.style.${index}.css`;
    const style = descriptor.styles[index];
    contents += `import ${JSON.stringify(
      './' + path.basename(styleFilename),
    )};`;

    const result = await compileStyleAsync({
      id,
      filename: styleFilename,
      source: style.content,
      postcssPlugins: [
        postcssPresetEnv({
          features: {
            'nesting-rules': true,
          },
        }),
      ],
      preprocessLang: style.lang,
      scoped: style.scoped,
    });

    await fs.promises.writeFile(styleFilename, result.code);
  }
  const renderFuncName = ssr ? 'ssrRender' : 'render';
  const templateFilename = inputFilename + '.template.js';
  contents += `import { ${renderFuncName} } from ${JSON.stringify(
    './' + path.basename(templateFilename),
  )}; component.${renderFuncName} = ${renderFuncName};`;
  if (descriptor.styles.some((o) => o.scoped)) {
    contents += `component.__scopeId = ${JSON.stringify(dataId)};`;
  }
  if (ssr) {
    contents += 'component.__ssrInlineRender = true;';
  }

  contents += 'export default component;';
  await fs.promises.writeFile(inputFilename + '.js', contents);

  const result = compileTemplate({
    id,
    filename: inputFilename,
    source: descriptor.template.content,
    scoped: hasScoped,
    slotted: descriptor.slotted,
    ssr,
    ssrCssVars: [],
    isProd,
    compilerOptions: {
      inSSR: ssr,
      bindingMetadata: script?.bindings,
      runtimeModuleName,
    },
  });

  await fs.promises.writeFile(templateFilename, result.code);
}

run();
