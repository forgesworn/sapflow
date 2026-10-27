export const PLAYER_VERSION='2026.10.27.10';

export function newerPlayerVersion(value){return typeof value==='string'&&/^[0-9.]{1,32}$/.test(value)&&value.localeCompare(PLAYER_VERSION,'en',{numeric:true})>0;}
