// The ESM detector must ignore "import"/"export" inside comments, strings,
// template literals and regex literals. This file is a classic plugin.

// import fake from 'not-a-real-module';
/* export const alsoFake = 1; */
const s1 = "import foo from 'bar'";
const s2 = 'export default 1';
const t = `import ${'x'} from 'y'`;
const re = /import|export/;
const division = 10 / 2;

let n = 0;

host.log(
  'comment classic loaded',
  s1.length,
  s2.length,
  t.length,
  re.test('import'),
  division,
);

function on_tick() {
  n += 1;
  host.log('comment classic tick', n);
}
