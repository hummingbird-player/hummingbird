UPDATE track SET location = $1, folder = $2 WHERE source = 'local' AND location = $3;
