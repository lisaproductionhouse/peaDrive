$Flavor = 'personal'

Get-Content ".env.$Flavor" | ForEach-Object { 
    if ($_ -match '=') { 
        $k,$v = $_ -split '=',2; 
        Set-Item "env:$k" $v 
    } 
}

$extra = if ($Flavor -eq 'personal') { 
    '--features','full-drive','--config','src-tauri/tauri.personal.conf.json' 
} else { 
    @() 
}

cargo tauri build @extra