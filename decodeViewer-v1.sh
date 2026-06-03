#!/bin/sh
dir=$1
TAB=${2-TAB}
AWK=${AWK-awk}

usage()
{
	echo
	echo
	echo $0 data_directory separator
	echo
	echo	"    separator: [,|TAB]"
	echo
	exit
}

if [ $# -lt 1 ]; then
	usage
fi

#if [ ! -d "${dir}*" ]; then
	#echo "ERROR: Directory [$dir] does not exist!"
	#exit 1
#fi
go()
{
  if [ -f $dir ]; then
    echo $dir|grep \.gz$ > /dev/null
    if [ $? -eq 0 ]; then
      $CB_GZIP -dc $dir
    else
      cat $dir
    fi
  else
    for d in `ls -d ${dir}*`
    do
      #for i in `find ${dir}* -follow -type f| sort`
      for i in `find $d -follow -type f| sort`
      do
        echo $i|grep \.gz$ > /dev/null
        if [ $? -eq 0 ]; then
          $CB_GZIP -dc $i
        else
          cat $i
        fi
      done
    done
  fi
}

go|$AWK -v TAB="$TAB" '
BEGIN {
	#printf("TAB=<%s>",TAB);
	if(TAB=="")
		FS=","
	else if (TAB=="TAB") FS="\t"; 
	else FS=TAB;
	i=1
	EXEC[i++]="format_ver";
	EXEC[i++]="rec_type";
	EXEC[i++]="db_type";
	EXEC[i++]="utc";
	EXEC[i++]="time_flag";
	EXEC[i++]="dest_ip";
	EXEC[i++]="dest_port";
	EXEC[i++]="client_ip";
	EXEC[i++]="client_port";
	EXEC[i++]="client_host";
	EXEC[i++]="client_mac";
	EXEC[i++]="sql_conn_id";
	EXEC[i++]="sql_id";
	EXEC[i++]="seq_num";
	EXEC[i++]="server_name";
	EXEC[i++]="instance_name";
	EXEC[i++]="db_name";
	EXEC[i++]="db_version";
	EXEC[i++]="db_user";
	EXEC[i++]="os_user";
	EXEC[i++]="tty";

	EXEC[i++]="sql_client_program";
	EXEC[i++]="sql_stmt_id";
	EXEC[i++]="sql_exec_id";

	EXEC[i++]="ap_client_user";
	EXEC[i++]="ap_client_host";
	EXEC[i++]="ap_client_program";

	EXEC[i++]="n_host_variable";
	EXEC[i++]="bind_vars";
	EXEC[i++]="sql_hash";
	EXEC[i++]="format";
	EXEC[i++]="sql_stmt";
	EXEC[i++]="sql_class";
	EXEC[i++]="tbs";
	EXEC[i++]="cols";
	EXEC[i++]="funs";
	EXEC[i++]="vars";
	EXEC[i++]="pid";
	EXEC[i++]="sql_word";
	EXEC[i++]="tb_group_mask";
	EXEC[i++]="conn_utc";
	EXEC[i++]="capt_src";
	EXEC[i++]="sql_inject";
	EXEC[i++]="sen_vars0";
	EXEC[i++]="sen_vars1";
	EXEC[i++]="sen_vars2";
	EXEC[i++]="ap_sys";
	#EXEC[i++]="country";
	#EXEC[i++]="region";
	#EXEC[i++]="city";
	EXEC_CNT=i-1;

	i=1
	CONN[i++]="format_ver";
	CONN[i++]="rec_type";
	CONN[i++]="db_type";
	CONN[i++]="utc";
	CONN[i++]="time_flag";
	CONN[i++]="dest_ip";
	CONN[i++]="dest_port";
	CONN[i++]="client_ip";
	CONN[i++]="client_port";
	CONN[i++]="client_host";
	CONN[i++]="client_mac";
	CONN[i++]="sql_conn_id";
	CONN[i++]="server_name";
	CONN[i++]="instance_name";
	CONN[i++]="db_name";
	CONN[i++]="db_version";
	CONN[i++]="db_user";
	CONN[i++]="os_user";
	CONN[i++]="tty";
	CONN[i++]="sql_client_program";
	CONN[i++]="pid";
	CONN[i++]="login_status";
	CONN[i++]="format";
	CONN[i++]="ap_sys";
	CONN_CNT=i-1;

	i=1
	SQL[i++]="format_ver";
	SQL[i++]="rec_type";
	SQL[i++]="db_type";
	SQL[i++]="utc";
	SQL[i++]="time_flag";
	SQL[i++]="dest_ip";
	SQL[i++]="dest_port";
	SQL[i++]="client_ip";
	SQL[i++]="client_port";
	SQL[i++]="client_host";
	SQL[i++]="client_mac";
	SQL[i++]="sql_stmt_id";
	SQL[i++]="server_name";
	SQL[i++]="instance_name";
	SQL[i++]="db_name";
	SQL[i++]="db_version";
	SQL[i++]="db_user";
	SQL[i++]="os_user";
	SQL[i++]="tty";
	SQL[i++]="sql_client_program";
	SQL[i++]="sql_stmt_id";
	SQL[i++]="sql_id";
	SQL[i++]="sql_error_code";
	SQL[i++]="sql_statement";
	SQL[i++]="format";
	SQL[i++]="sql_hash";
	SQL[i++]="sql_class";
	SQL[i++]="tbs";
	SQL[i++]="cols";
	SQL[i++]="funs";
	SQL[i++]="ap_sys";


	i=1
	EXECTIME[i++]="format_ver";
	EXECTIME[i++]="rec_type";
	EXECTIME[i++]="db_type";
	EXECTIME[i++]="utc";
	EXECTIME[i++]="time_flag";
	EXECTIME[i++]="sql_conn_id";
	EXECTIME[i++]="sql_id";
	EXECTIME[i++]="seq_num";
	EXECTIME[i++]="sql_error_code";
	EXECTIME[i++]="sql_exec_status";
	EXECTIME[i++]="row_estimated";
	EXECTIME[i++]="row_effected";
	EXECTIME[i++]="response_time";
	EXECTIME[i++]="elapse_time";
	#EXECTIME[i++]="format";
	EXECTIME[i++]="login_status";
	EXECTIME[i++]="sqlClosed";
	#EXECTIME[i++]="ext1";
	EXECTIME_CNT=i-1;


	i=1
	ROWDATA[i++]="format_ver";
	ROWDATA[i++]="rec_type";
	ROWDATA[i++]="db_type";
	ROWDATA[i++]="utc";
	ROWDATA[i++]="time_flag";
	ROWDATA[i++]="dest_ip";
	ROWDATA[i++]="dest_port";
	ROWDATA[i++]="client_ip";
	ROWDATA[i++]="client_port";
	ROWDATA[i++]="client_host";
	ROWDATA[i++]="client_mac";
	ROWDATA[i++]="sql_conn_id";
	ROWDATA[i++]="sql_id";
	ROWDATA[i++]="seq_num";
	ROWDATA[i++]="row_no";
	ROWDATA[i++]="server_name";
	ROWDATA[i++]="instance_name";
	ROWDATA[i++]="db_name";
	ROWDATA[i++]="db_user";
	ROWDATA[i++]="os_user";
	ROWDATA[i++]="tty";

	ROWDATA[i++]="sql_client_program";

	ROWDATA[i++]="ap_client_user";
	ROWDATA[i++]="ap_client_host";
	ROWDATA[i++]="ap_client_program";
	ROWDATA[i++]="tbs";

	ROWDATA[i++]="data";
	ROWDATA[i++]="conn_utc";
	ROWDATA[i++]="sql_utc";
	ROWDATA[i++]="ap_sys";
	ROWDATA_CNT=i-1;
	
        i=1
	URI[i++]="ver"
	URI[i++]="rec"
	URI[i++]="utc"
	URI[i++]="time_flag"
	URI[i++]="web_exec_id"
	URI[i++]="hash"
	URI[i++]="dir"
	URI[i++]="header"
	URI[i++]="method_type"
	URI[i++]="uri_value"
	URI[i++]="client_ip"
	URI[i++]="client_port"
	URI[i++]="dest_ip"
	URI[i++]="dest_port"
	URI[i++]="client_mac"
	URI[i++]="tran_no"
	URI[i++]="proto"
	URI[i++]="sess_id"
	URI[i++]="ap_user"
	URI[i++]="login_status"
	URI[i++]="is_login_page"
	URI[i++]="params"
	URI[i++]="dest_host"
	URI[i++]="extend_col_1"
	URI[i++]="extend_col_2"
	URI[i++]="extend_col_3"
	URI[i++]="web_status"
	URI[i++]="country"
	URI[i++]="region"
	URI[i++]="city"
	URI[i++]="ap_sys"

	URI_CNT=i-1;



	 INCORRECT=" ==> INCORRECT!"
}
(NR==1 && TAB=="" ) && /\\t.*\\t.*\\t/{
	FS="\t"
}

function getVal(key,idx) {
	if (key == "EXEC") return EXEC[idx]; 
	#if (key == "SQL") return SQL[idx]; 
	#if (key == "CONN") return CONN[idx]; 
	if (key == "EXECTIME") return EXECTIME[idx]; 
	if (key == "ROWDATA") return ROWDATA[idx]; 
	if (key == "URI") return URI[idx]; 
}

function fieldChk(fd,v) {
	if(fd=="sql_class"){
		if(v!="DML"&& v!="DDL"&&v!="DCL"&&v!="OTHER"&&v!="CONNECT"&&v!="DISCONNECT"&&v!="SPL"&&v!="TCL"){
			return INCORRECT
		} else return "";
	}else if(fd=="ap_sys"){ if(v==""){ return INCORRECT } else return ""; }
	else if(fd=="conn_utc"){ if(v=="0"){ return INCORRECT } else return ""; }
	else if(fd=="client_ip"){ client_ip=v; return "";}
	else if(fd=="client_port"){ client_port=v; return "";}
	else if(fd=="sql_stmt"){ if(v==""){ return INCORRECT } else return ""; }
	else if(fd=="db_user"){ if(!match(v,"^[a-zA-Z]")){ return INCORRECT } else return ""; }
	#else if(fd=="os_user"){ if(!match(v,"^[a-zA-Z]")){ return INCORRECT } else return ""; }
	## Could be Chinese value
	else if(fd=="client_host"){ if(v!="" && !match(v,"^[0-9a-zA-Z]")){ return INCORRECT } else return ""; }
	#else if(fd=="client_host"){ if(v!="" && !isalpha(v)){ return INCORRECT } else return ""; }
	#else if(fd=="server_name"){ if(!match(tolower(v),"^lnka")){ return INCORRECT } else return ""; }
	#else if(fd=="server_name"){ if(v!="" && !match(v,"^[a-zA-Z]")){ return INCORRECT } else return ""; }
	else if(fd=="server_name"){ if(v==""){ return INCORRECT } else return ""; }
	#else if(fd=="instance_name"){ if(v!="" && !match(v,"^[a-zA-Z]")){ return INCORRECT } else return ""; }
	else if(fd=="instance_name"){ if(v==""){ return INCORRECT } else return ""; }
	else if(fd=="tty"){ if(v!="" && !match(v,"^[/a-zA-Z0-9]")){ return INCORRECT } else return ""; }
	else if(fd=="sql_client_program"){ if(v!="" && substr(v,1,4)!=".Net" && !match(v,"^[/\\\\a-zA-Z]")){ return INCORRECT } else return ""; }
	else if(fd=="sql_hash"){ if(v==""){ return INCORRECT } else return ""; }
	else if(fd=="sql_conn_id"){ if(v==""){ return INCORRECT } else return ""; }
	else if(fd=="sql_id"){ if(v==""){ return INCORRECT } else return ""; }
	else if(fd=="seq_num"){ if(!match(v,"[0-9]+")){ return INCORRECT } else return ""; }
	else if(fd=="pid"){ if(!match(v,"[0-9]+")){ return INCORRECT } else return ""; }
	else if(fd=="utc"){ if(v<1000000000 || v > 4000000000){ return INCORRECT } else return ""; }
	return "";
}

#||($2=="SQL") \
#||($2=="CONN") \

($2=="EXEC") \
||($2=="ROWDATA") \
||($2=="EXECTIME") \
||($2=="URI") \
{
#print
	printf("\n\n");
	if($2=="EXEC" && NF!=EXEC_CNT){
		printf("\n\n@@ERROR== INCORRECT ================\n");
	} else if($2=="ROWDATA" && NF!=ROWDATA_CNT){
		printf("\n\n@@ERROR==================\n");
	} else if($2=="EXECTIME" && NF!=EXECTIME_CNT){
		printf("\n\n@@ERROR==================\n");
	} else if($2=="URI" && NF!=URI_CNT){
		printf("\n\n@@ERROR==================\n");
	}
	printf("%s--------------------------\n",$2);
	result="";
	for(i=1;i<=NF;i++){
		field=getVal($2,i);
		result=fieldChk(field,$i);
		if(result!=""){
			result=sprintf(" INCORRECT!  src %s:%s",client_ip,client_port);
		}
		printf("%s\t%3d <%s> --> <%s>%s\n",$2,i,field,$i,result);
	}
}
'
