// 声明Polkadot SDK公开配置和唯一SDK自动化工作流。

export const manifest=Object.freeze({
  "schema": 1,
  "product_id": "polkadot-sdk",
  "configuration": {
    "id": "polkadot-sdk",
    "kind": "sdk",
    "title": "Polkadot SDK配置",
    "items": []
  }
});
export function describeConfiguration(){return JSON.parse(JSON.stringify(manifest));}

// 本产品Workflow坐标声明。
export const workflows=Object.freeze({"sdk":"sdk.yml"});

// 只返回声明数据；派发与观察归通用调用方，版本及交付归本仓YAML。
export function describeAction({flow,platform,inputs={}}){
 const workflow=workflows[platform];
 if(flow==='release'&&workflow)return {repository:"crcfrcn/polkadot-sdk",workflow,ref:'main',inputs};
 throw Error('本产品未声明该流程：'+flow+'·'+platform);
}
