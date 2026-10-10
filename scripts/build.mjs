#!/usr/bin/env node
// Polkadot SDK唯一完整本机编译入口；只消费本仓锁与声明，不调用控制台或兄弟仓流程。
import {writeFileSync} from 'node:fs';
import {spawn} from 'node:child_process';
import {AsyncLocalStorage} from 'node:async_hooks';
import {Socket} from 'node:net';
import {createHash,randomUUID} from 'node:crypto';
import {readFile,writeFile,mkdir,lstat,realpath,readdir,rm,rmdir,copyFile,rename,chmod} from 'node:fs/promises';
import {dirname,join,resolve,relative,isAbsolute,sep} from 'node:path';
import {fileURLToPath} from 'node:url';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
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
export function testRoot(){return join(root,'target/test');}
export function checkWork(work){if(work!==join(root,'target/build/sdk')&&work!==testRoot())fail('工作根必须归本仓target/build/sdk或target/test');return work;}
async function directory(path){await mkdir(path,{recursive:true,mode:0o700});if(await realpath(path)!==path||!(await lstat(path)).isDirectory())fail('工作路径经过链接或非目录');return path;}
export async function requirements(platform,work){checkWork(work);if(platform!=='sdk'||process.platform!==contract.bootstrap.platform||process.arch!==contract.bootstrap.arch)fail('本机平台没有声明实现');await readFile(join(root,'Cargo.lock'));return {schema:1,product_id:contract.product_id,platform,tools:contract.platforms.sdk.tools,locks:contract.platforms.sdk.locks,apple:contract.apple,bootstrap:contract.bootstrap};}
async function run(file,args,{cwd,env,signal,capture=false}={}){
 signal?.throwIfAborted();return new Promise((ok,reject)=>{let reason,closed=false,output=[];const child=spawn(file,args,{cwd,env,detached:true,stdio:['ignore','pipe','pipe']});const state=task.getStore(),work=[join(root,'target/build/sdk'),testRoot()].find(path=>cwd===path||cwd?.startsWith(path+sep));const groups=work?(recipeGroups.get(work)||new Set()):new Set();if(work)recipeGroups.set(work,groups);const record=()=>{if(work)writeFileSync(join(work,'.resource-active.json'),JSON.stringify({pid:process.pid,groups:[...groups]})+'\n');};if(Number.isSafeInteger(child.pid)){groups.add(child.pid);state?.groups.add(child.pid);state?.onGroups?.(state.groups);record();}
 let killer;const stop=()=>{reason??=Error('编译已取消');try{process.kill(-child.pid,'SIGTERM');}catch{}killer??=setTimeout(()=>{try{process.kill(-child.pid,'SIGKILL');}catch{}},1500);};signal?.addEventListener('abort',stop,{once:true});
 let size=0;for(const stream of [child.stdout,child.stderr])stream.on('data',bytes=>{size+=bytes.length;if(size>16*1024**2){reason=Error('工具输出超限');stop();}else if(capture)output.push(bytes);else process.stderr.write(bytes);});
 child.once('error',e=>{reason=e;});child.once('close',async code=>{closed=true;signal?.removeEventListener('abort',stop);clearTimeout(killer);const alive=()=>{if(!Number.isSafeInteger(child.pid))return false;try{process.kill(-child.pid,0);return true;}catch(e){return e.code!=='ESRCH';}};if(alive()){try{process.kill(-child.pid,'SIGTERM');}catch{}for(let n=0;n<15&&alive();n++)await new Promise(resolve=>setTimeout(resolve,100));if(alive())try{process.kill(-child.pid,'SIGKILL');}catch{}for(let n=0;n<15&&alive();n++)await new Promise(resolve=>setTimeout(resolve,100));}if(alive())return reject(Error('工具后代仍运行，保留现场'));groups.delete(child.pid);record();state?.groups.delete(child.pid);state?.onGroups?.(state.groups);reason||code!==0?reject(reason||Error('工具执行失败，退出码'+code)):ok(Buffer.concat(output).toString());});
 });
}
export function assertWorkQuiescent(work=task.getStore()?.work){for(const pid of new Set([...(task.getStore()?.groups||[]),...(recipeGroups.get(work)||[])]))try{process.kill(-pid,0);fail('工具进程退出未确认');}catch(e){if(e.code!=='ESRCH')throw e;}}
async function bootstrapTools(work,signal){for(const name of ['tar','shell']){const file=contract.bootstrap[name];if(await realpath(file)!==file||!(await lstat(file)).isFile())fail('Apple自举入口无效');}return {tar:contract.bootstrap.tar,shell:contract.bootstrap.shell};}
async function standaloneOriginal(entry,{work,offline,signal}){const dir=await directory(join(work,'originals')),file=join(dir,entry.sha256+'.blob');try{const bytes=await readFile(file);if(hash(bytes)!==entry.sha256)fail('原件与本仓声明不符');return file;}catch(e){if(e.code!=='ENOENT')throw e;}if(offline)fail('离线缺少声明原件');const url=new URL(entry.url);if(url.protocol!=='https:'||url.username||url.password)fail('原件必须为公开HTTPS');const response=await fetch(url,{signal});if(!response.ok)fail('原件获取失败');const bytes=Buffer.from(await response.arrayBuffer());if(!bytes.length||bytes.length>4*1024**3||hash(bytes)!==entry.sha256)fail('原件与声明不符');await writeFile(file,bytes,{flag:'wx',mode:0o444});return file;}
export async function prepareToolSupply(tool,{original,payload,work,signal,acquireOriginal,acquireApple}){
 const bootstrap=await bootstrapTools(work,signal),environment={PATH:'/usr/bin:/bin',HOME:work,TMPDIR:work};await directory(payload);
 if(tool.archive.kind==='binary'){await directory(dirname(join(payload,tool.archive.executable)));await copyFile(original,join(payload,tool.archive.executable));await chmod(join(payload,tool.archive.executable),0o555);}
 else{const unpack=await directory(join(work,'unpack-'+tool.id+'-'+randomUUID()));try{
  const listing=await run(bootstrap.tar,['-tf',original],{cwd:work,env:environment,signal,capture:true});for(const name of listing.trim().split('\n'))if(name.startsWith('/')||name.split('/').includes('..'))fail('官方工具归档路径越界');
  await run(bootstrap.tar,['-xf',original,'-C',unpack],{cwd:work,env:environment,signal});const source=tool.archive.root==='.'?unpack:join(unpack,tool.archive.root);
  if(tool.archive.kind==='rust'){
   await run(bootstrap.shell,[join(source,'install.sh'),'--prefix='+payload,'--disable-ldconfig','--components=rustc,cargo,rust-std-aarch64-apple-darwin,rust-src,rustfmt-preview,clippy-preview'],{cwd:work,env:environment,signal});
   for(const component of tool.components||[]){const file=await acquireOriginal(component,{kind:'tool'}),at=await directory(join(unpack,component.target));await run(bootstrap.tar,['-xf',file,'-C',at],{cwd:work,env:environment,signal});await run(bootstrap.shell,[join(at,component.root,'install.sh'),'--prefix='+payload,'--disable-ldconfig'],{cwd:work,env:environment,signal});}
  }else{for(const name of await readdir(source))await rename(join(source,name),join(payload,name));}
 }finally{assertWorkQuiescent(work);await rm(unpack,{recursive:true,force:true});}}
 return {schema:1,id:tool.id,version:tool.version,payload,original};
}
export async function prepareResourceSupply(platform,work,previous,options={}){
 if(options.provided===true&&['acquireTool','acquireApple','runCommand'].some(name=>typeof options[name]!=='function'))fail('调度供给能力不完整');
 const plan=await requirements(platform,work),tools={},signal=options.signal,environment={HOME:work,TMPDIR:join(work,'tmp'),PATH:''};await directory(environment.TMPDIR);
 for(const tool of plan.tools){signal?.throwIfAborted();if(options.acquireTool)tools[tool.id]=await options.acquireTool(tool);else{const original=await standaloneOriginal(tool.archive,{work,offline:options.offline,signal}),payload=await directory(join(work,'tools',tool.id));await prepareToolSupply(tool,{original,payload,work,signal,acquireOriginal:e=>standaloneOriginal(e,{work,offline:options.offline,signal})});tools[tool.id]={version:tool.version,path:join(payload,tool.archive.executable)};}}
 let apple;if(options.acquireApple)apple=await options.acquireApple(plan.apple);else{const developerDirectory=(await run('/usr/bin/xcode-select',['-p'],{cwd:work,env:environment,signal,capture:true})).trim();const names={};for(const name of plan.apple.names)names[name]=(await run('/usr/bin/xcrun',['--find',name],{cwd:work,env:{...environment,DEVELOPER_DIR:developerDirectory},signal,capture:true})).trim();apple={developerDirectory,tools:names};}
 const cargo=join(dirname(tools.rust.path),'cargo'),cargoHome=await directory(join(work,'dependencies/cargo')),cargoTarget=await directory(join(work,'work/cargo-target'));
 Object.assign(environment,{CARGO_HOME:cargoHome,CARGO_TARGET_DIR:cargoTarget,RUSTC:tools.rust.path,DEVELOPER_DIR:apple.developerDirectory,CC:apple.tools.clang,CXX:apple.tools['clang++'],AR:apple.tools.ar,PROTOC:tools.protoc.path,SOLC:tools.solc.path,RESOLC:tools.resolc.path,CMAKE:tools.cmake.path,PATH:[...new Set(Object.values(tools).map(t=>dirname(t.path))),dirname(apple.tools.make)].join(':')});
 // Cargo只按本仓原始锁准备依赖；随后构建强制离线，不改锁、不跟随新版本。
 const prepareCommand=options.provided===true?options.runCommand:run;if(typeof prepareCommand!=='function')fail('调度供给缺少依赖执行能力');
 await prepareCommand(cargo,['fetch','--manifest-path',join(root,'Cargo.toml'),'--locked',...(options.offline?['--offline']:[])],{cwd:work,env:environment,signal});
 return {schema:1,product_id:contract.product_id,platform,work,run_id:previous.run_id,offline:true,tools,environment,dependencies:{own:{cargoHome}},archives:{}};
}
export function resourceEnvironment(platform,work,value){checkWork(work);if(platform!=='sdk'||value?.schema!==1||value.product_id!==contract.product_id||value.platform!==platform||value.work!==work||value.offline!==true)fail('资源回执身份无效');for(const tool of contract.platforms.sdk.tools)if(value.tools?.[tool.id]?.version!==tool.version)fail('资源版本与本仓声明不符');return value.environment;}
async function suppliedResources(platform,work,request,signal){const stream=new Socket({fd:4,readable:true,writable:true}),plan=await requirements(platform,work);return new Promise((ok,reject)=>{let text='',done=false;const finish=(error,value)=>{if(done)return;done=true;signal?.removeEventListener('abort',abort);stream.destroy();error?reject(error):ok(value);},abort=()=>finish(Error('资源供给已取消'));signal?.throwIfAborted();signal?.addEventListener('abort',abort,{once:true});stream.setEncoding('utf8');stream.on('data',chunk=>{text+=chunk;if(Buffer.byteLength(text)>2*1024**2)return finish(Error('供给回执超限'));if(!text.includes('\n'))return;try{const [line,extra]=text.split('\n');if(extra)fail('供给回执不是唯一帧');const reply=JSON.parse(line);if(reply.id!=='1'||reply.ok!==true||reply.value?.run_id!==request.run_id)fail('供给回执失败或任务不符');finish(null,reply.value);}catch(e){finish(e);}});stream.on('error',e=>finish(e));stream.on('end',()=>finish(Error('供给通道中断')));stream.write(JSON.stringify({id:'1',operation:'prepare',previous:request})+'\n');});}
// 领取与收尾共用本仓短锁，直到全部子内容删除后才释放。
async function withWorkClaim(work,action){
 checkWork(work);await directory(work);const path=work===join(root,'target/build/sdk')?join(root,'target/build/.claim-sdk'):join(work,'.claim.lock');await mkdir(path,{mode:0o700});const held=await lstat(path);
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
 if(run_id&&work===join(root,'target/build/sdk'))try{await lstat(work);}catch(error){if(error.code==='ENOENT')return;throw error;}
 const value=await withWorkClaim(work,async()=>{
  const lock=join(work,'.active.json');let owner;try{const info=await lstat(lock);if(!info.isFile()||info.isSymbolicLink()||info.nlink!==1)fail('编译守卫无效');owner=JSON.parse(await readFile(lock,'utf8'));}catch(error){if(error.code==='ENOENT'){if(work===join(root,'target/build/sdk')&&(await readdir(work)).length===0)await rmdir(work);return;}throw error;}
  if(owner.work!==work||owner.product_id!==contract.product_id||owner.run_id!==run_id||!Number.isSafeInteger(owner.pid)||owner.pid<2||!Array.isArray(owner.groups)||owner.groups.some(pid=>!Number.isSafeInteger(pid)||pid<2))fail('编译收尾身份无效');
  for(const pid of [owner.pid,...owner.groups.map(pid=>-pid)])try{process.kill(pid,0);fail('产品进程退出未确认');}catch(error){if(error.code!=='ESRCH')throw error;}
  assertWorkQuiescent(work);await clearBuildContents(work);if(work===join(root,'target/build/sdk'))await rmdir(work);
 });return value;
}
export async function execute(platform,work,request={},options={}){
 options.signal?.throwIfAborted();checkWork(work);await directory(work);await requirements(platform,work);if(!request||typeof request!=='object'||Array.isArray(request)||Object.keys(request).some(k=>k!=='run_id')||request.run_id!==undefined&&(typeof request.run_id!=='string'||!/^[1-9][0-9]{8}$/u.test(request.run_id)))fail('SDK Build只接受本轮运行编号');
 const provided=process.env.PRODUCT_RESOURCE_FD==='4';
 const runID=request.run_id||randomUUID(),mode=provided?'provided':'independent',lock=join(work,'.active.json');const owner={schema:1,product_id:contract.product_id,platform,scope:'build',work,run_id:runID,pid:process.pid,nonce:randomUUID(),groups:[]};await withWorkClaim(work,async()=>{await assertSupplyExited(work);await writeFile(lock,JSON.stringify(owner)+'\n',{flag:'wx',mode:0o600});});let unsafe=false;
 const cancellation=new AbortController(),abort=()=>cancellation.abort();options.signal?.addEventListener('abort',abort,{once:true});
 try{return await task.run({work,groups:new Set(),onGroups:groups=>writeFileSync(lock,JSON.stringify({...owner,groups:[...groups]})+'\n')},async()=>{const receipt=mode==='provided'?await suppliedResources(platform,work,{run_id:runID},cancellation.signal):await prepareResourceSupply(platform,work,{run_id:runID},{...options,provided:false});const env=resourceEnvironment(platform,work,receipt);await build(platform,work,receipt,{signal:cancellation.signal});assertWorkQuiescent();return {schema:1,product_id:contract.product_id,platform,work,run_id:runID,completion:'compile-only',files:[]};});}
 catch(e){unsafe=String(e.message).includes('退出未确认')||String(e.message).includes('后代仍运行');throw e;}finally{options.signal?.removeEventListener('abort',abort);if(!unsafe&&mode==='independent')await withWorkClaim(work,async()=>{await clearBuildContents(work);await rmdir(work);});}
}

export async function build(platform,work,receipt,{signal,runner=run}={}){
 signal?.throwIfAborted();const env=resourceEnvironment(platform,work,receipt);
 await runner(join(dirname(receipt.tools.rust.path),'cargo'),['build','--manifest-path',join(root,'Cargo.toml'),'--workspace','--all-targets','--release','--locked','--offline'],{cwd:work,env:{...env,CARGO_NET_OFFLINE:'true'},signal});
 return {schema:1,product_id:contract.product_id,platform,work,run_id:receipt.run_id,completion:'compile-only',files:[]};
}

if(process.argv[1]===fileURLToPath(import.meta.url)&&!process.env.NODE_TEST_CONTEXT){const [command,platform,flag,work,...extra]=process.argv.slice(2);if(command!=='execute'||flag!=='--work'||extra.some(x=>x!=='--offline')||extra.length>1)fail('本机编译参数无效');let input='';for await(const chunk of process.stdin){input+=chunk;if(Buffer.byteLength(input)>2*1024**2)fail('公开输入超限');}const cancellation=new AbortController();for(const s of ['SIGINT','SIGTERM'])process.once(s,()=>cancellation.abort());process.stdout.write(JSON.stringify(await execute(platform,work,input?JSON.parse(input):{},{offline:extra.includes('--offline'),signal:cancellation.signal}))+'\n');}

// BEGIN INLINE TESTS
if(process.env.NODE_TEST_CONTEXT&&process.argv[1]===import.meta.filename){
 const {test}=await import('node:test'),{default:assert}=await import('node:assert/strict');
 test('SDK平台编译现场位于独立目录且空现场收尾删除目录',async()=>{
  const work=join(root,'target/build/sdk');assert.equal(checkWork(work),work);
  assert.throws(()=>checkWork(join(root,'target/build')),/工作根/);
  await directory(work);await finishBuild(work,{run_id:'123456789'});
  await assert.rejects(lstat(work),{code:'ENOENT'});
 });
 const receipt=()=>({schema:1,product_id:contract.product_id,platform:'sdk',work:testRoot(),run_id:'123456789',offline:true,environment:{},tools:Object.fromEntries(contract.platforms.sdk.tools.map(t=>[t.id,{version:t.version,path:'/fixture/'+t.id+'/bin/'+(t.id==='rust'?'rustc':t.id)}]))});
 test('SDK编译使用本仓原始锁，完整工作区和离线依赖',async()=>{const value=receipt();let called=0;await build('sdk',testRoot(),value,{runner:async(file,args,options)=>{called++;assert.equal(file,'/fixture/rust/bin/cargo');assert.ok(args.includes('--workspace')&&args.includes('--all-targets')&&args.includes('--locked')&&args.includes('--offline'));assert.equal(options.env.CARGO_NET_OFFLINE,'true');assert.equal(options.cwd,testRoot());}});assert.equal(called,1);});
 test('SDK工具版本不符、取消或真实执行失败不能产生成功',async()=>{const value=receipt();value.tools.rust.version='invalid';let called=false;await assert.rejects(build('sdk',testRoot(),value,{runner:()=>{called=true;}}),/版本/);assert.equal(called,false);const abort=new AbortController();abort.abort();await assert.rejects(build('sdk',testRoot(),receipt(),{signal:abort.signal,runner:()=>assert.fail('取消不得执行')}));await assert.rejects(build('sdk',testRoot(),receipt(),{runner:()=>{throw Error('fixture compiler failure');}}),/fixture compiler failure/);});
 test('SDK调度依赖只使用交付执行能力，缺能力及执行失败不回退',async()=>{
  const work=testRoot();await mkdir(dirname(work),{recursive:true});await mkdir(work);
  try{
   const previous={run_id:'123456789'};let calls=0;
   const options={provided:true,acquireTool:async tool=>({version:tool.version,path:'/fixture/'+tool.id+'/bin/'+(tool.id==='rust'?'rustc':tool.id)}),acquireApple:async plan=>({developerDirectory:'/fixture/Xcode',tools:Object.fromEntries(plan.names.map(name=>[name,'/fixture/Apple/'+name]))}),runCommand:async(file,args,settings)=>{calls++;assert.equal(file,'/fixture/rust/bin/cargo');assert.equal(args[0],'fetch');assert.ok(args.includes('--locked'));assert.equal(settings.cwd,work);}};
   const result=await prepareResourceSupply('sdk',work,previous,options);assert.equal(calls,1);assert.equal(result.offline,true);
   await assert.rejects(prepareResourceSupply('sdk',work,previous,{...options,runCommand:undefined}),/能力不完整/);assert.equal(calls,1);
   await assert.rejects(prepareResourceSupply('sdk',work,previous,{...options,runCommand:async()=>{throw Error('fixture fetch failure');}}),/fixture fetch failure/);
  }finally{await rm(work,{recursive:true,force:true});}
 });

 test('SDK领取与收尾互斥，异任务及资源后代不能清场',async()=>{
  const work=testRoot();await mkdir(dirname(work),{recursive:true});await mkdir(work);
  try{
   await withWorkClaim(work,async()=>{
    await assert.rejects(finishBuild(work,{run_id:'123456789'}),/EEXIST/);
    await assert.rejects(execute('sdk',work,{run_id:'123456789'}),/EEXIST/);
   });
   const {spawnSync}=await import('node:child_process');const pid=spawnSync(process.execPath,['-e','']).pid;
   const owner={schema:1,product_id:contract.product_id,work,run_id:'123456789',pid,groups:[]};await writeFile(join(work,'.active.json'),JSON.stringify(owner));await writeFile(join(work,'keep'),'owned');
   await assert.rejects(finishBuild(work,{run_id:'other'}),/身份/);
   // 当前Node不一定是组长，使用活跃供应方PID确定性验证退出边界。
   await writeFile(join(work,'.supply-active.json'),JSON.stringify({pid:process.ppid,groups:[]}));
   await assert.rejects(finishBuild(work,{run_id:'123456789'}),/退出未确认/);assert.equal(await readFile(join(work,'keep'),'utf8'),'owned');
   await rm(join(work,'.supply-active.json'));await finishBuild(work,{run_id:'123456789'});assert.deepEqual(await readdir(work),[]);
  }finally{await rm(work,{recursive:true,force:true});}
 });

}
// END INLINE TESTS
