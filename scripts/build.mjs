#!/usr/bin/env node
// 准备Polkadot SDK编译所需的工具、资源和独占工作目录。
// 执行工作区编译，并维护自动化运行记录及缓存。

import {writeFileSync,existsSync,readdirSync,rmdirSync} from 'node:fs';
import {spawn} from 'node:child_process';
import {AsyncLocalStorage} from 'node:async_hooks';
import {createHash,randomUUID} from 'node:crypto';
import {readFile,writeFile,mkdir,lstat,realpath,readdir,rm,rmdir,copyFile,rename,chmod} from 'node:fs/promises';
import {dirname,join,resolve,relative,isAbsolute,sep} from 'node:path';
import {fileURLToPath} from 'node:url';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
// 本仓测试用随机会话映射固定夹具；正式Build始终领取原平台根。
if(process.env.NODE_TEST_CONTEXT&&!process.env.POLKADOT_SDK_TEST_SESSION)process.env.POLKADOT_SDK_TEST_SESSION=randomUUID();
const testSession=process.env.NODE_TEST_CONTEXT&&/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/u.test(process.env.POLKADOT_SDK_TEST_SESSION||'')?process.env.POLKADOT_SDK_TEST_SESSION:null;
export const contract=JSON.parse(await readFile(join(root,'scripts/flows.json'),'utf8'));
const recipeGroups=new Map();
const task=new AsyncLocalStorage(),fail=message=>{throw Error('Polkadot SDK编译：'+message);};
export async function polkadotRelease(release,platform,readTag){
 if(platform!=='sdk')fail('Polkadot SDK只有完整源码包发布目标');
 const tag=release?.tag_name;
 if(typeof tag!=='string'||!tag.startsWith('polkadot-sdk-sdk-v'))return null;
 const sequence=/^polkadot-sdk-sdk-v([0-9]+\.[0-9]+\.[0-9]+)-r([1-9][0-9]*)-a([1-9][0-9]*)$/u.exec(tag);
 if(sequence===null)fail('Polkadot SDK正式版本序列无效');
 const run=BigInt(sequence[2]),attempt=BigInt(sequence[3]);
 if(run>BigInt(Number.MAX_SAFE_INTEGER)||attempt>BigInt(Number.MAX_SAFE_INTEGER))fail('Polkadot SDK运行序列不可准确表示');
 const commit=await readTag(tag);
 if(commit?.ref!==`refs/tags/${tag}`||commit.object?.type!=='commit'||!/^[a-f0-9]{40}$/u.test(commit.object.sha||''))fail('Polkadot SDK正式包没有准确上游源码提交');
 return {version:sequence[1],run_id:Number(run),run_attempt:Number(attempt),tag,source_sha:commit.object.sha};
}
const inside=(base,path)=>path===base||path.startsWith(base+sep),hash=bytes=>createHash('sha256').update(bytes).digest('hex');
export function testRoot(){return testSession?join(root,'target/test/sessions',testSession):join(root,'target/test');}
function buildWork(){return testSession?join(testRoot(),'build/sdk'):join(root,'target/build/sdk');}
export function checkWork(work){if(work!==buildWork()&&work!==testRoot())fail('工作根必须归本仓本轮Build或测试会话');return work;}
async function directory(path){await mkdir(path,{recursive:true,mode:0o700});if(await realpath(path)!==path||!(await lstat(path)).isDirectory())fail('工作路径经过链接或非目录');return path;}
export async function requirements(platform,work){checkWork(work);if(platform!=='sdk'||process.platform!==contract.bootstrap.platform||process.arch!==contract.bootstrap.arch)fail('本机平台没有声明实现');await readFile(join(root,'Cargo.lock'));return {schema:1,product_id:contract.product_id,platform,tools:contract.platforms.sdk.tools.map(tool=>({...tool,cache_key:tool.id==='node'?tool.archive.sha256:tool.archive.sha256+'-'+hash(JSON.stringify([tool,prepareToolSupply.toString()]))})),locks:contract.platforms.sdk.locks,apple:contract.apple,bootstrap:contract.bootstrap};}
async function run(file,args,{cwd,env,signal,capture=false}={}){
 signal?.throwIfAborted();return new Promise((ok,reject)=>{let reason,closed=false,output=[];const child=spawn(file,args,{cwd,env,detached:true,stdio:['ignore','pipe','pipe']});const state=task.getStore(),work=[buildWork(),testRoot()].find(path=>cwd===path||cwd?.startsWith(path+sep));const groups=work?(recipeGroups.get(work)||new Set()):new Set();if(work)recipeGroups.set(work,groups);const record=()=>{if(work)writeFileSync(join(work,'.resource-active.json'),JSON.stringify({pid:process.pid,groups:[...groups]})+'\n');};if(Number.isSafeInteger(child.pid)){groups.add(child.pid);state?.groups.add(child.pid);state?.onGroups?.(state.groups);record();}
 let killer;const stop=()=>{reason??=Error('编译已取消');try{process.kill(-child.pid,'SIGTERM');}catch{}killer??=setTimeout(()=>{try{process.kill(-child.pid,'SIGKILL');}catch{}},1500);};signal?.addEventListener('abort',stop,{once:true});
 let size=0;for(const stream of [child.stdout,child.stderr])stream.on('data',bytes=>{size+=bytes.length;if(size>16*1024**2){reason=Error('工具输出超限');stop();}else if(capture)output.push(bytes);else process.stderr.write(bytes);});
 child.once('error',e=>{reason=e;});child.once('close',async code=>{closed=true;signal?.removeEventListener('abort',stop);clearTimeout(killer);const alive=()=>{if(!Number.isSafeInteger(child.pid))return false;try{process.kill(-child.pid,0);return true;}catch(e){return e.code!=='ESRCH';}};if(alive()){try{process.kill(-child.pid,'SIGTERM');}catch{}for(let n=0;n<15&&alive();n++)await new Promise(resolve=>setTimeout(resolve,100));if(alive())try{process.kill(-child.pid,'SIGKILL');}catch{}for(let n=0;n<15&&alive();n++)await new Promise(resolve=>setTimeout(resolve,100));}if(alive())return reject(Error('工具后代仍运行，保留现场'));groups.delete(child.pid);record();state?.groups.delete(child.pid);state?.onGroups?.(state.groups);reason||code!==0?reject(reason||Error('工具执行失败，退出码'+code)):ok(Buffer.concat(output).toString());});
 });
}
export function assertWorkQuiescent(work=task.getStore()?.work){for(const pid of new Set([...(task.getStore()?.groups||[]),...(recipeGroups.get(work)||[])]))try{process.kill(-pid,0);fail('工具进程退出未确认');}catch(e){if(e.code!=='ESRCH')throw e;}}
async function bootstrapTools(work,signal){
 const tools={};for(const name of ['tar','shell']){const file=contract.bootstrap[name],actual=await realpath(file);
  if(actual!==file&&!(file==='/usr/bin/tar'&&actual==='/usr/bin/bsdtar')||!(await lstat(actual)).isFile())fail('Apple自举入口无效');tools[name]=actual;
 }return tools;
}
async function standaloneOriginal(entry,{work,offline,signal}){const dir=await directory(join(work,'originals')),file=join(dir,entry.sha256+'.blob');try{const bytes=await readFile(file);if(hash(bytes)!==entry.sha256)fail('原件与本仓声明不符');return file;}catch(e){if(e.code!=='ENOENT')throw e;}if(offline)fail('离线缺少声明原件');const url=new URL(entry.url);if(url.protocol!=='https:'||url.username||url.password)fail('原件必须为公开HTTPS');const response=await fetch(url,{signal});if(!response.ok)fail('原件获取失败');const bytes=Buffer.from(await response.arrayBuffer());if(!bytes.length||bytes.length>4*1024**3||hash(bytes)!==entry.sha256)fail('原件与声明不符');await writeFile(file,bytes,{flag:'wx',mode:0o444});return file;}
// 组件语义仅归SDK；供应端不理解rust-src与目标标准库的布局。
async function validateSDKTool(tool,payload){
 const base=await realpath(payload);if(base!==payload)fail('SDK工具根经过链接');
 for(const slot of tool.slots){const file=await realpath(join(base,slot)),info=await lstat(file);if(!inside(base,file)||!info.isFile()||!(info.mode&0o111))fail('SDK工具入口缺失或越界');}
 for(const component of tool.components||[]){
  if(component.target==='rust-src')await rustLibraryManifest(join(base,tool.archive.executable));
  else {const path=await realpath(join(base,'lib/rustlib',component.target,'lib'));if(!inside(base,path)||!(await lstat(path)).isDirectory())fail('SDK目标标准库缺失或越界');}
 }
}
async function acceptSDKTool(tool,value){
 const suffix='/'+tool.archive.executable;
 if(value?.version!==tool.version||value.id!==undefined&&value.id!==tool.id||typeof value.path!=='string'||!value.path.endsWith(suffix))fail('SDK工具交付身份不符');
 const payload=value.root||value.path.slice(0,-suffix.length);if(!isAbsolute(payload)||value.path!==join(payload,tool.archive.executable))fail('SDK工具交付路径不符');
 await validateSDKTool(tool,payload);return value;
}
export async function prepareToolSupply(tool,{payload,work,signal,acquireOriginal,acquireApple}){
 const original=await acquireOriginal(tool.archive,{kind:'tool'});
 const bootstrap=await bootstrapTools(work,signal),environment={PATH:'/usr/bin:/bin',HOME:work,TMPDIR:work};await directory(payload);
 if(tool.archive.kind==='binary'){await directory(dirname(join(payload,tool.archive.executable)));await copyFile(original,join(payload,tool.archive.executable));await chmod(join(payload,tool.archive.executable),0o555);}
 else{const unpack=await directory(join(work,'unpack-'+tool.id+'-'+randomUUID()));try{
  const listing=await run(bootstrap.tar,['-tf',original],{cwd:work,env:environment,signal,capture:true});for(const name of listing.trim().split('\n'))if(name.startsWith('/')||name.split('/').includes('..'))fail('官方工具归档路径越界');
  await run(bootstrap.tar,['-xf',original,'-C',unpack],{cwd:work,env:environment,signal});const source=tool.archive.root==='.'?unpack:join(unpack,tool.archive.root);
  if(tool.archive.kind==='rust'){
   await run(bootstrap.shell,[join(source,'install.sh'),'--prefix='+payload,'--disable-ldconfig','--components=rustc,cargo,rust-std-aarch64-apple-darwin,rustfmt-preview,clippy-preview'],{cwd:work,env:environment,signal});
   for(const component of tool.components||[]){const file=await acquireOriginal(component,{kind:'tool'}),at=await directory(join(unpack,component.target));await run(bootstrap.tar,['-xf',file,'-C',at],{cwd:work,env:environment,signal});await run(bootstrap.shell,[join(at,component.root,'install.sh'),'--prefix='+payload,'--disable-ldconfig'],{cwd:work,env:environment,signal});}
  }else{for(const name of await readdir(source))await rename(join(source,name),join(payload,name));}
 }finally{assertWorkQuiescent(work);await rm(unpack,{recursive:true,force:true});}}
 await validateSDKTool(tool,payload);return {schema:1,id:tool.id,version:tool.version,payload};
}
export async function standaloneApple(plan,work,environment,signal,execute=run){
 if(typeof plan.sdk!=='string'||!/^[a-z0-9][a-z0-9.-]{0,63}$/u.test(plan.sdk))fail('SDK未声明Apple SDK目标');
 const developerDirectory=(await execute('/usr/bin/xcode-select',['-p'],{cwd:work,env:environment,signal,capture:true})).trim(),tools={};
 for(const name of plan.names)tools[name]=name==='xcrun'?'/usr/bin/xcrun':(await execute('/usr/bin/xcrun',['--find',name],{cwd:work,env:{...environment,DEVELOPER_DIR:developerDirectory},signal,capture:true})).trim();
 const sdk=(await execute('/usr/bin/xcrun',['--sdk',plan.sdk,'--show-sdk-path'],{cwd:work,env:{...environment,DEVELOPER_DIR:developerDirectory},signal,capture:true})).trim();
 return {developerDirectory,tools,sdk};
}
// 只接受当前Xcode内的SDK与宿主libclang，链接器和动态库不从PATH或其它安装取得。
export async function checkedAppleCargo(apple){
 if(typeof apple?.developerDirectory!=='string'||!isAbsolute(apple.developerDirectory)||typeof apple.sdk!=='string'||!isAbsolute(apple.sdk)||typeof apple?.tools?.clang!=='string'||!isAbsolute(apple.tools.clang))fail('SDK Apple工具路径无效');
 try{
  const developerDirectory=await realpath(apple.developerDirectory),sdk=await realpath(apple.sdk),clang=await realpath(apple.tools.clang);
  if(!inside(developerDirectory,sdk)||!inside(developerDirectory,clang)||!(await lstat(sdk)).isDirectory()||!(await lstat(clang)).isFile())fail('SDK Apple工具越界');
  const system=await realpath(join(sdk,'usr/lib/libSystem.tbd'));
  if(!inside(sdk,system)||!(await lstat(system)).isFile())fail('SDK macOS系统库无效');
  const libclang=await realpath(join(dirname(dirname(clang)),'lib/libclang.dylib')),info=await lstat(libclang);
  if(!inside(developerDirectory,libclang)||!info.isFile()||info.nlink!==1)fail('SDK libclang无效');
  return {developerDirectory,sdk,clang,libclangDirectory:dirname(libclang)};
 }catch(error){if(String(error?.message||'').startsWith('Polkadot SDK编译：'))throw error;fail('SDK Apple工具缺失或越界');}
}
// build-std读取所交付Rust工具自己的原锁；不能只准备产品Cargo.lock后离线猜包。
export async function rustLibraryManifest(rustc){
 if(typeof rustc!=='string'||!isAbsolute(rustc))fail('SDK Rust工具入口无效');
 const root=dirname(dirname(rustc)),library=join(root,'lib/rustlib/src/rust/library');
 try{
  for(const name of ['Cargo.toml','Cargo.lock']){
   const file=join(library,name),actual=await realpath(file),info=await lstat(actual);
   if(!inside(root,actual)||!info.isFile()||info.nlink!==1)fail('SDK Rust工具原锁越界或无效');
  }
  return join(library,'Cargo.toml');
 }catch(error){if(String(error?.message||'').startsWith('Polkadot SDK编译：'))throw error;fail('SDK Rust工具原锁缺失');}
}
export async function prepareResourceSupply(platform,work,previous,options={}){
 const plan=await requirements(platform,work),tools={},signal=options.signal,environment={HOME:work,TMPDIR:join(work,'tmp'),PATH:''};await directory(environment.TMPDIR);
 for(const tool of plan.tools){signal?.throwIfAborted();{const payload=await directory(join(work,'tools',tool.id));await prepareToolSupply(tool,{payload,work,signal,acquireOriginal:e=>standaloneOriginal(e,{work,offline:options.offline,signal})});tools[tool.id]=await acceptSDKTool(tool,{version:tool.version,path:join(payload,tool.archive.executable)});}}
 const apple=await standaloneApple(plan.apple,work,environment,signal);
 const appleCargo=await checkedAppleCargo(apple);
 const cargo=join(dirname(tools.rust.path),'cargo'),cargoHome=await directory(join(work,'dependencies/cargo')),cargoTarget=await directory(join(work,'work/cargo-target'));
 // Rust链接、系统库和bindgen均使用本轮同一Xcode；只交给当前Cargo任务。
 Object.assign(environment,{CARGO_HOME:cargoHome,CARGO_TARGET_DIR:cargoTarget,RUSTC:tools.rust.path,DEVELOPER_DIR:appleCargo.developerDirectory,SDKROOT:appleCargo.sdk,CC:appleCargo.clang,CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER:appleCargo.clang,CXX:apple.tools['clang++'],AR:apple.tools.ar,LIBCLANG_PATH:appleCargo.libclangDirectory,DYLD_LIBRARY_PATH:appleCargo.libclangDirectory,PROTOC:tools.protoc.path,SOLC:tools.solc.path,RESOLC:tools.resolc.path,CMAKE:tools.cmake.path,PATH:[...new Set(Object.values(tools).map(t=>dirname(t.path))),dirname(apple.tools.make)].join(':')});
 // Cargo只按本仓原始锁准备依赖；随后构建强制离线，不改锁、不跟随新版本。
 const prepareCommand=run;
 const rustLibrary=await rustLibraryManifest(tools.rust.path);
 await prepareCommand(cargo,['fetch','--manifest-path',join(root,'Cargo.toml'),'--locked',...(options.offline?['--offline']:[])],{cwd:work,env:environment,signal});
 await prepareCommand(cargo,['fetch','--manifest-path',rustLibrary,'--locked',...(options.offline?['--offline']:[])],{cwd:work,env:{...environment,RUSTC_BOOTSTRAP:'1'},signal});
 return {schema:1,product_id:contract.product_id,platform,work,run_id:previous.run_id,offline:true,tools,environment,dependencies:{own:{cargoHome}},archives:{}};
}
export function resourceEnvironment(platform,work,value){checkWork(work);if(platform!=='sdk'||value?.schema!==1||value.product_id!==contract.product_id||value.platform!==platform||value.work!==work||value.offline!==true)fail('资源回执身份无效');for(const tool of contract.platforms.sdk.tools)if(value.tools?.[tool.id]?.version!==tool.version)fail('资源版本与本仓声明不符');return value.environment;}

// 领取与收尾共用本仓短锁，直到全部子内容删除后才释放。
async function withWorkClaim(work,action){
 checkWork(work);await directory(work);const path=work===buildWork()?join(dirname(work),'.claim-sdk'):join(work,'.claim.lock');await mkdir(path,{mode:0o700});const held=await lstat(path);
 try{return await action();}finally{const current=await lstat(path);if(current.dev!==held.dev||current.ino!==held.ino)fail('工作短锁漂移');await rm(path,{recursive:true});}
}
async function assertSupplyExited(work){
 for(const name of ['.resource-active.json','.supply-active.json']){
  const path=join(work,name);let info;try{info=await lstat(path);}catch(error){if(error.code==='ENOENT')continue;throw error;}
  if(!info.isFile()||info.isSymbolicLink()||info.nlink!==1||info.size>65536)fail('资源退出记录无效');const value=JSON.parse(await readFile(path,'utf8'));
  if(!Number.isSafeInteger(value.pid)||value.pid<2||!Array.isArray(value.groups)||value.groups.some(pid=>!Number.isSafeInteger(pid)||pid<2))fail('资源进程身份无效');
  for(const pid of [...(value.pid===process.pid?[]:[value.pid]),...value.groups.map(pid=>-pid)])try{process.kill(pid,0);fail('资源工具退出未确认');}catch(error){if(error.code!=='ESRCH')throw error;}
 }
}
async function clearBuildContents(work){
 await assertSupplyExited(work);for(const name of await readdir(work))if(!['.claim.lock','.active.json'].includes(name))await rm(join(work,name),{recursive:true,force:true});await rm(join(work,'.active.json'),{force:true});
}
export async function finishBuild(work,{run_id}={}){
 if(run_id&&work===buildWork())try{await lstat(work);}catch(error){if(error.code==='ENOENT')return;throw error;}
 const value=await withWorkClaim(work,async()=>{
  const lock=join(work,'.active.json');let owner;try{const info=await lstat(lock);if(!info.isFile()||info.isSymbolicLink()||info.nlink!==1)fail('编译守卫无效');owner=JSON.parse(await readFile(lock,'utf8'));}catch(error){if(error.code==='ENOENT'){if(work===buildWork()&&(await readdir(work)).length===0)await rmdir(work);return;}throw error;}
  if(owner.work!==work||owner.product_id!==contract.product_id||owner.run_id!==run_id||!Number.isSafeInteger(owner.pid)||owner.pid<2||!Array.isArray(owner.groups)||owner.groups.some(pid=>!Number.isSafeInteger(pid)||pid<2))fail('编译收尾身份无效');
  for(const pid of [owner.pid,...owner.groups.map(pid=>-pid)])try{process.kill(pid,0);fail('产品进程退出未确认');}catch(error){if(error.code!=='ESRCH')throw error;}
  assertWorkQuiescent(work);await clearBuildContents(work);if(work===buildWork())await rmdir(work);
 });return value;
}
// 完整Build由本仓在资源回执后运行自身测试，再进入正式编译。
function testFiles(){return ['scripts/build.mjs'];}
async function runProductTests(platform,work,receipt,signal){
 const node=receipt.tools.node,expected=contract.platforms.sdk.tools.find(value=>value.id==='node');
 if(!node||node.version!==expected?.version||!isAbsolute(node.path)||await realpath(node.path)!==node.path)fail('SDK测试缺少准确Node入口');
 const files=testFiles().map(file=>join(root,file));
 for(const file of files){const info=await lstat(file);if(!info.isFile()||info.isSymbolicLink()||await realpath(file)!==file)fail('SDK测试文件无效');}
 const digests=await Promise.all(files.map(async file=>hash(await readFile(file))));
 // Node测试运行器自行设置测试子进程身份；预设NODE_TEST_CONTEXT会跳过测试文件。
 await run(node.path,['--test','--test-concurrency=1',...files],
  {cwd:root,env:{HOME:work,LANG:'C',LC_ALL:'C',PATH:receipt.environment.PATH},signal});
 for(let index=0;index<files.length;index++)if(hash(await readFile(files[index]))!==digests[index])fail('SDK测试期间源码改变');
}

export async function execute(platform,work,request={},options={}){
 options.signal?.throwIfAborted();checkWork(work);await directory(work);await requirements(platform,work);if(!request||typeof request!=='object'||Array.isArray(request)||Object.keys(request).some(k=>k!=='run_id')||request.run_id!==undefined&&(typeof request.run_id!=='string'||!/^[1-9][0-9]{8}$/u.test(request.run_id)))fail('SDK Build只接受本轮运行编号');
 const runID=request.run_id||randomUUID(),lock=join(work,'.active.json');const owner={schema:1,product_id:contract.product_id,platform,scope:'build',work,run_id:runID,pid:process.pid,nonce:randomUUID(),groups:[]};await withWorkClaim(work,async()=>{await assertSupplyExited(work);await writeFile(lock,JSON.stringify(owner)+'\n',{flag:'wx',mode:0o600});});let unsafe=false;
 const cancellation=new AbortController(),abort=()=>cancellation.abort();options.signal?.addEventListener('abort',abort,{once:true});
 try{return await task.run({work,groups:new Set(),onGroups:groups=>writeFileSync(lock,JSON.stringify({...owner,groups:[...groups]})+'\n')},async()=>{const receipt=await prepareResourceSupply(platform,work,{run_id:runID},{...options,signal:cancellation.signal});const env=resourceEnvironment(platform,work,receipt);await runProductTests(platform,work,receipt,cancellation.signal);await build(platform,work,receipt,{signal:cancellation.signal});assertWorkQuiescent();return {schema:1,product_id:contract.product_id,platform,work,run_id:runID,completion:'compile-only',files:[]};});}
 catch(e){unsafe=String(e.message).includes('退出未确认')||String(e.message).includes('后代仍运行');throw e;}finally{options.signal?.removeEventListener('abort',abort);if(!unsafe)await withWorkClaim(work,async()=>{await clearBuildContents(work);await rmdir(work);});}
}

const sdkRepository='crcfrcn/polkadot-sdk';
const sdkWorkflow='.github/workflows/sdk.yml';
// Run与Cargo缓存使用不同的GitHub Actions资源接口；身份始终绑定本仓main和单一sdk.yml。
function sdkRetentionIdentity(environment){
 if(environment.GITHUB_ACTIONS!=='true'||environment.GITHUB_REPOSITORY!==sdkRepository
  ||environment.GITHUB_EVENT_NAME!=='workflow_dispatch'||environment.GITHUB_REF!=='refs/heads/main'
  ||environment.GITHUB_WORKFLOW_REF!==sdkRepository+'/'+sdkWorkflow+'@refs/heads/main'
  ||!/^[a-f0-9]{40}$/u.test(environment.GITHUB_SHA||'')
  ||!['GITHUB_RUN_ID','GITHUB_RUN_NUMBER','GITHUB_RUN_ATTEMPT'].every(key=>/^[1-9][0-9]*$/u.test(environment[key]||'')&&Number.isSafeInteger(Number(environment[key])))
  ||!environment.GH_TOKEN)fail('GitHub SDK保留清理身份无效');
}
function sdkRunBucket(run){return run.conclusion==='success'?'success':'failure';}
function sdkRunOrder(left,right){return right.run_number-left.run_number||right.run_attempt-left.run_attempt||right.id-left.id;}
function sdkValidateRun(run,{current=false,environment}){
 if(!Number.isSafeInteger(run?.id)||run.id<1||run.path!==sdkWorkflow
  ||run.repository?.full_name!==sdkRepository||run.head_branch!=='main'
  ||!/^[a-f0-9]{40}$/u.test(run.head_sha||'')
  ||!Number.isSafeInteger(run.run_number)||run.run_number<1
  ||!Number.isSafeInteger(run.run_attempt)||run.run_attempt<1
  ||current&&(run.id!==Number(environment.GITHUB_RUN_ID)||run.run_number!==Number(environment.GITHUB_RUN_NUMBER)
    ||run.run_attempt!==Number(environment.GITHUB_RUN_ATTEMPT)||run.head_sha!==environment.GITHUB_SHA||run.status!=='in_progress')
  ||!current&&(run.status!=='completed'||typeof run.conclusion!=='string'||!run.conclusion))fail('GitHub SDK Run保留候选身份无效');
 return run;
}
async function sdkGitHubRequest(path,{environment=process.env,fetchImpl=fetch,method='GET'}={}){
 let response;try{response=await fetchImpl('https://api.github.com/repos/'+sdkRepository+'/'+path,{method,redirect:'error',
  headers:{Accept:'application/vnd.github+json',Authorization:'Bearer '+environment.GH_TOKEN,'X-GitHub-Api-Version':'2026-03-10'},
  signal:AbortSignal.timeout(30_000)});}catch{fail('GitHub SDK保留清理请求失败');}
 if(method==='DELETE'&&response.status===204)return null;
 if(!response.ok)fail('GitHub SDK保留清理接口失败（HTTP '+response.status+'）');
 try{return await response.json();}catch{fail('GitHub SDK保留清理响应无效');}
}
async function sdkWorkflowRuns({environment=process.env,fetchImpl=fetch}={}){
 const runs=[];
 for(let page=1;page<=10000;page++){
  const data=await sdkGitHubRequest('actions/workflows/sdk.yml/runs?per_page=100&page='+page,{environment,fetchImpl});
  if(!Array.isArray(data?.workflow_runs))fail('GitHub SDK Run分页响应无效');
  runs.push(...data.workflow_runs);if(data.workflow_runs.length<100)return runs;
 }
 fail('GitHub SDK Run分页超过上限');
}
export async function polkadotRunPrune(outcome,environment=process.env,{fetchImpl=fetch}={}){
 if(!['success','failure'].includes(outcome))fail('GitHub SDK Run清理结果无效');sdkRetentionIdentity(environment);
 const current=sdkValidateRun(await sdkGitHubRequest('actions/runs/'+environment.GITHUB_RUN_ID,{environment,fetchImpl}),{current:true,environment});
 const all=await sdkWorkflowRuns({environment,fetchImpl});
 for(const run of all){if(run?.repository?.full_name!==sdkRepository)fail('GitHub SDK Run列表仓库不符');
  if(run.path!==sdkWorkflow||run.head_branch!=='main'||run.status!=='completed')continue;
  sdkValidateRun(run,{environment});}
 const completed=all.filter(run=>run.path===sdkWorkflow&&run.head_branch==='main'&&run.status==='completed');
 if(completed.some(run=>run.id===current.id)||completed.some(run=>run.run_number>=current.run_number))fail('GitHub SDK Run顺序或活动身份不一致');
 // 当前Run仍在运行，以前置完整流程结果作为本轮候选；只删本仓main本Workflow的旧Run。
 const opposite=completed.filter(run=>sdkRunBucket(run)!==outcome).sort(sdkRunOrder)[0];
 const keep=new Set([current.id,...(opposite?[opposite.id]:[])]),deleted=[];
 for(const run of completed)if(!keep.has(run.id)){await sdkGitHubRequest('actions/runs/'+run.id,{environment,fetchImpl,method:'DELETE'});deleted.push(run.id);}
 const remaining=(await sdkWorkflowRuns({environment,fetchImpl})).filter(run=>run.path===sdkWorkflow&&run.head_branch==='main'&&run.status==='completed');
 const same=remaining.filter(run=>sdkRunBucket(run)===outcome),other=remaining.filter(run=>sdkRunBucket(run)!==outcome);
 if(same.length||other.length!==(opposite?1:0)||opposite&&other[0]?.id!==opposite.id)fail('GitHub SDK Run保留清理未收敛');
 const verifiedCurrent=sdkValidateRun(await sdkGitHubRequest('actions/runs/'+current.id,{environment,fetchImpl}),{current:true,environment});
 if(verifiedCurrent.id!==current.id)fail('GitHub SDK活动Run身份改变');
 process.stdout.write('GitHub SDK Run清理成功：候选='+outcome+'，删除旧Run='+deleted.length+'，保留另一结果='+other.length+'\n');
 return {outcome,run_id:current.id,deleted:deleted.length,retained_other:other.length};
}
export async function polkadotCachePrune(environment=process.env,{fetchImpl=fetch}={}){
 sdkRetentionIdentity(environment);
 const prefix='polkadot-sdk-cargo-v1-macOS-ARM64-sdk-',key=environment.CARGO_CACHE_KEY;
 const pattern=new RegExp('^'+prefix+'[a-f0-9]{64}-[1-9][0-9]*-[1-9][0-9]*$','u');
 if(typeof key!=='string'||!pattern.test(key)
  ||!key.endsWith('-'+environment.GITHUB_RUN_ID+'-'+environment.GITHUB_RUN_ATTEMPT))fail('Polkadot SDK Cargo缓存键无效');
 async function list(){const rows=[];for(let page=1;page<=1000;page++){
  const data=await sdkGitHubRequest('actions/caches?ref=refs%2Fheads%2Fmain&per_page=100&page='+page,{environment,fetchImpl});
  if(!Array.isArray(data?.actions_caches))fail('GitHub SDK Cargo缓存列表无效');
  rows.push(...data.actions_caches);if(data.actions_caches.length<100)return rows.filter(item=>item.ref==='refs/heads/main'&&typeof item.key==='string'&&item.key.startsWith(prefix));
 }fail('GitHub SDK Cargo缓存分页超过上限');}
 const caches=await list();for(const item of caches)if(!Number.isSafeInteger(item.id)||item.id<1||!pattern.test(item.key))fail('Polkadot SDK Cargo缓存候选无效');
 const selected=caches.find(item=>item.key===key)??[...caches].sort((a,b)=>Date.parse(b.created_at)-Date.parse(a.created_at)||b.id-a.id)[0]??null;
 const deleted=[];for(const item of caches)if(item.id!==selected?.id){await sdkGitHubRequest('actions/caches/'+item.id,{environment,fetchImpl,method:'DELETE'});deleted.push(item.id);}
 const remaining=await list();if(remaining.length!==(selected?1:0)||selected&&remaining[0].id!==selected.id)fail('Polkadot SDK Cargo缓存未收敛到一份');
 process.stdout.write('Polkadot SDK Cargo缓存清理成功：保留='+remaining.length+'，删除旧缓存='+deleted.length+'\n');
 return {retained:remaining.length,deleted:deleted.length,key:selected?.key??null};
}

export async function build(platform,work,receipt,{signal,runner=run}={}){
 signal?.throwIfAborted();const env=resourceEnvironment(platform,work,receipt);
 await runner(join(dirname(receipt.tools.rust.path),'cargo'),['build','--manifest-path',join(root,'Cargo.toml'),'--workspace','--all-targets','--release','--locked','--offline'],{cwd:work,env:{...env,CARGO_NET_OFFLINE:'true'},signal});
 return {schema:1,product_id:contract.product_id,platform,work,run_id:receipt.run_id,completion:'compile-only',files:[]};
}

if(process.argv[1]===fileURLToPath(import.meta.url)&&!process.env.NODE_TEST_CONTEXT){
 const [command,...args]=process.argv.slice(2);
 if(command==='sdk-cache-prune'&&args.length===0)process.stdout.write(JSON.stringify(await polkadotCachePrune())+'\n');
 else if(command==='sdk-run-prune'&&args.length===1)process.stdout.write(JSON.stringify(await polkadotRunPrune(args[0]))+'\n');
 else if(command==='execute'){
  const [platform,flag,work,...extra]=args;
  if(flag!=='--work'||extra.some(x=>x!=='--offline')||extra.length>1)fail('本机编译参数无效');
  let input='';for await(const chunk of process.stdin){input+=chunk;if(Buffer.byteLength(input)>2*1024**2)fail('公开输入超限');}
  const cancellation=new AbortController();for(const s of ['SIGINT','SIGTERM'])process.once(s,()=>cancellation.abort());
  process.stdout.write(JSON.stringify(await execute(platform,work,input?JSON.parse(input):{},{offline:extra.includes('--offline'),signal:cancellation.signal}))+'\n');
 }else fail('本机编译参数无效');
}
