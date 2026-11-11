// Render SVGs to PNGs with Chromium. usage: node render.js in.svg out.png size [in out size ...]
// usage: node render.js in.svg out.png size
const {chromium}=require('playwright');const fs=require('fs');const path=require('path');
(async()=>{const b=await chromium.launch();const p=await b.newPage({viewport:{width:1,height:1}});
const jobs=[];for(let i=2;i<process.argv.length;i+=3)jobs.push(process.argv.slice(i,i+3));
const font='data:font/ttf;base64,'+fs.readFileSync(path.join(__dirname,'../../assets/fonts/8823Gothic-Regular.ttf')).toString('base64');
for(const [inp,out,s] of jobs){const n=+s;await p.setViewportSize({width:n,height:n});
const svg=fs.readFileSync(inp,'utf8');
await p.setContent(`<style>@font-face{font-family:G;src:url(${font})}html,body{margin:0;background:transparent}svg{width:${n}px;height:${n}px;display:block}</style>${svg}`);
await p.evaluate(()=>document.fonts.ready);
await p.screenshot({path:out,omitBackground:true});}
await b.close();})();
