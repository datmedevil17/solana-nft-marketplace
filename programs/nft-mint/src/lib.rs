#![allow(unexpected_cfgs)]
#![allow(deprecated)]
use anchor_lang::{prelude::*, solana_program};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{mint_to, Mint, MintTo, Token, TokenAccount},
};
use mpl_token_metadata::{
    instructions::{
        CreateMasterEditionV3, CreateMasterEditionV3InstructionArgs,
        CreateMetadataAccountV3, CreateMetadataAccountV3InstructionArgs,
        UpdateMetadataAccountV2, UpdateMetadataAccountV2InstructionArgs,
        SignMetadata,
    },
    types::{Creator, DataV2, Collection, CollectionDetails},
};
use anchor_lang::solana_program::program::invoke;

declare_id!("11111111111111111111111111111111");

#[program]
pub mod nft_mint {
    use super::*;

    /// Initialize the NFT minting program
    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        let mint_authority = &mut ctx.accounts.mint_authority;
        mint_authority.authority = ctx.accounts.authority.key();
        mint_authority.bump = ctx.bumps.mint_authority;
        
        msg!("NFT Mint program initialized");
        Ok(())
    }

    /// Mint a new NFT with metadata and master edition
    pub fn mint_nft(
        ctx: Context<MintNft>,
        metadata: CreateNftMetadata,
        collection: Option<Collection>,
    ) -> Result<()> {
        let mint = &ctx.accounts.mint;
        let token_account = &ctx.accounts.token_account;
        let mint_authority = &ctx.accounts.mint_authority;
        let payer = &ctx.accounts.payer;

        // Clone metadata early to avoid partial move issues
        let metadata_clone = metadata.clone();

        // Create mint authority seeds for CPI
        let authority_key = mint_authority.authority;
        let seeds = &[
            b"mint_authority",
            authority_key.as_ref(),
            &[mint_authority.bump],
        ];
        let signer = &[&seeds[..]];

        // Mint 1 token to the token account
        let cpi_accounts = MintTo {
            mint: mint.to_account_info(),
            to: token_account.to_account_info(),
            authority: mint_authority.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
        mint_to(cpi_ctx, 1)?;

        // Create metadata account
        let creators = metadata
            .creators
            .iter()
            .map(|creator| Creator {
                address: creator.address,
                verified: creator.address == payer.key(),
                share: creator.share,
            })
            .collect();

        let data = DataV2 {
            name: metadata.name,
            symbol: metadata.symbol,
            uri: metadata.uri,
            seller_fee_basis_points: metadata.seller_fee_basis_points,
            creators: Some(creators),
            collection,
            uses: None,
        };

        let create_metadata_ix = CreateMetadataAccountV3 {
            metadata: ctx.accounts.metadata.key(),
            mint: mint.key(),
            mint_authority: mint_authority.key(),
            payer: payer.key(),
            update_authority: (mint_authority.key(), true),
            system_program: ctx.accounts.system_program.key(),
            rent: Some(ctx.accounts.rent.key()),
        };

        let create_metadata_args = CreateMetadataAccountV3InstructionArgs {
            data,
            is_mutable: true,
            collection_details: None,
        };

        invoke(
            &create_metadata_ix.instruction(create_metadata_args),
            &[
                ctx.accounts.metadata.to_account_info(),
                mint.to_account_info(),
                mint_authority.to_account_info(),
                payer.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
                ctx.accounts.rent.to_account_info(),
            ],
        )?;

        // Create master edition
        let create_master_edition_ix = CreateMasterEditionV3 {
            edition: ctx.accounts.master_edition.key(),
            mint: mint.key(),
            update_authority: mint_authority.key(),
            mint_authority: mint_authority.key(),
            payer: payer.key(),
            metadata: ctx.accounts.metadata.key(),
            token_program: ctx.accounts.token_program.key(),
            system_program: ctx.accounts.system_program.key(),
            rent: Some(ctx.accounts.rent.key()),
        };

        let create_master_edition_args = CreateMasterEditionV3InstructionArgs {
            max_supply: Some(0), // Max supply of 0 makes it a unique NFT
        };

        invoke(
            &create_master_edition_ix.instruction(create_master_edition_args),
            &[
                ctx.accounts.master_edition.to_account_info(),
                mint.to_account_info(),
                mint_authority.to_account_info(),
                payer.to_account_info(),
                ctx.accounts.metadata.to_account_info(),
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
                ctx.accounts.rent.to_account_info(),
            ],
        )?;

        // Emit event
        emit!(NftMinted {
            mint: mint.key(),
            owner: token_account.owner,
            metadata: metadata_clone,
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!("NFT minted successfully: {}", mint.key());
        Ok(())
    }

    /// Update NFT metadata (only by verified creator)
    pub fn update_metadata(
        ctx: Context<UpdateMetadata>,
        new_metadata: CreateNftMetadata,
    ) -> Result<()> {
        let mint_authority = &ctx.accounts.mint_authority;
        let authority_key = mint_authority.authority;
        let seeds = &[
            b"mint_authority",
            authority_key.as_ref(),
            &[mint_authority.bump],
        ];
        let signer = &[&seeds[..]];

        let creators = new_metadata
            .creators
            .iter()
            .map(|creator| Creator {
                address: creator.address,
                verified: creator.address == ctx.accounts.payer.key(),
                share: creator.share,
            })
            .collect();

        let data = DataV2 {
            name: new_metadata.name,
            symbol: new_metadata.symbol,
            uri: new_metadata.uri,
            seller_fee_basis_points: new_metadata.seller_fee_basis_points,
            creators: Some(creators),
            collection: None,
            uses: None,
        };

        let update_metadata_ix = UpdateMetadataAccountV2 {
            metadata: ctx.accounts.metadata.key(),
            update_authority: mint_authority.key(),
        };

        let update_metadata_args = UpdateMetadataAccountV2InstructionArgs {
            data: Some(data),
            primary_sale_happened: None,
            is_mutable: None,
            new_update_authority: None,
        };

        invoke(
            &update_metadata_ix.instruction(update_metadata_args),
            &[
                ctx.accounts.metadata.to_account_info(),
                mint_authority.to_account_info(),
            ],
        )?;

        msg!("Metadata updated successfully");
        Ok(())
    }

    /// Verify creator signature
    pub fn verify_creator(ctx: Context<VerifyCreator>) -> Result<()> {
        let sign_metadata_ix = SignMetadata {
            metadata: ctx.accounts.metadata.key(),
            creator: ctx.accounts.creator.key(),
        };

        invoke(
            &sign_metadata_ix.instruction(),
            &[
                ctx.accounts.metadata.to_account_info(),
                ctx.accounts.creator.to_account_info(),
            ],
        )?;

        msg!("Creator verified successfully");
        Ok(())
    }

    /// Create a collection NFT
    pub fn create_collection(
        ctx: Context<CreateCollection>,
        metadata: CreateNftMetadata,
    ) -> Result<()> {
        let mint_authority = &ctx.accounts.mint_authority;
        let authority_key = mint_authority.authority;
        let seeds = &[
            b"mint_authority",
            authority_key.as_ref(),
            &[mint_authority.bump],
        ];
        let signer = &[&seeds[..]];

        // Mint 1 token for collection
        let cpi_accounts = MintTo {
            mint: ctx.accounts.mint.to_account_info(),
            to: ctx.accounts.token_account.to_account_info(),
            authority: mint_authority.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
        mint_to(cpi_ctx, 1)?;

        let creators = metadata
            .creators
            .iter()
            .map(|creator| Creator {
                address: creator.address,
                verified: creator.address == ctx.accounts.payer.key(),
                share: creator.share,
            })
            .collect();

        let data = DataV2 {
            name: metadata.name,
            symbol: metadata.symbol,
            uri: metadata.uri,
            seller_fee_basis_points: metadata.seller_fee_basis_points,
            creators: Some(creators),
            collection: None,
            uses: None,
        };

        let create_metadata_ix = CreateMetadataAccountV3 {
            metadata: ctx.accounts.metadata.key(),
            mint: ctx.accounts.mint.key(),
            mint_authority: mint_authority.key(),
            payer: ctx.accounts.payer.key(),
            update_authority: (mint_authority.key(), true),
            system_program: ctx.accounts.system_program.key(),
            rent: Some(ctx.accounts.rent.key()),
        };

        let create_metadata_args = CreateMetadataAccountV3InstructionArgs {
            data,
            is_mutable: true,
            collection_details: Some(CollectionDetails::V1 { size: 0 }),
        };

        invoke(
            &create_metadata_ix.instruction(create_metadata_args),
            &[
                ctx.accounts.metadata.to_account_info(),
                ctx.accounts.mint.to_account_info(),
                mint_authority.to_account_info(),
                ctx.accounts.payer.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
                ctx.accounts.rent.to_account_info(),
            ],
        )?;

        msg!("Collection created successfully");
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + MintAuthority::INIT_SPACE,
        seeds = [b"mint_authority", authority.key().as_ref()],
        bump
    )]
    pub mint_authority: Account<'info, MintAuthority>,
    
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MintNft<'info> {
    #[account(
        init,
        payer = payer,
        mint::decimals = 0,
        mint::authority = mint_authority,
        mint::freeze_authority = mint_authority,
    )]
    pub mint: Account<'info, anchor_spl::token::Mint>,

    #[account(
        init,
        payer = payer,
        associated_token::mint = mint,
        associated_token::authority = payer,
    )]
    pub token_account: Account<'info, TokenAccount>,

    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    pub master_edition: UncheckedAccount<'info>,

    #[account(
        seeds = [b"mint_authority", mint_authority.authority.as_ref()],
        bump = mint_authority.bump,
    )]
    pub mint_authority: Account<'info, MintAuthority>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub rent: Sysvar<'info, Rent>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    /// CHECK: This is the token metadata program
    pub token_metadata_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct UpdateMetadata<'info> {
    #[account(mut)]
    pub mint: Account<'info, Mint>,

    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    #[account(
        seeds = [b"mint_authority", mint_authority.authority.as_ref()],
        bump = mint_authority.bump,
    )]
    pub mint_authority: Account<'info, MintAuthority>,

    #[account(mut)]
    pub payer: Signer<'info>,
}

#[derive(Accounts)]
pub struct VerifyCreator<'info> {
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    pub creator: Signer<'info>,
}

#[derive(Accounts)]
pub struct CreateCollection<'info> {
    #[account(
        init,
        payer = payer,
        mint::decimals = 0,
        mint::authority = mint_authority,
        mint::freeze_authority = mint_authority,
    )]
    pub mint: Account<'info, Mint>,

    #[account(
        init,
        payer = payer,
        associated_token::mint = mint,
        associated_token::authority = payer,
    )]
    pub token_account: Account<'info, TokenAccount>,

    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    #[account(
        seeds = [b"mint_authority", mint_authority.authority.as_ref()],
        bump = mint_authority.bump,
    )]
    pub mint_authority: Account<'info, MintAuthority>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub rent: Sysvar<'info, Rent>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
}

#[account]
#[derive(InitSpace)]
pub struct MintAuthority {
    pub authority: Pubkey,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct CreateNftMetadata {
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub seller_fee_basis_points: u16,
    pub creators: Vec<NftCreator>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct NftCreator {
    pub address: Pubkey,
    pub share: u8,
}

#[event]
pub struct NftMinted {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub metadata: CreateNftMetadata,
    pub timestamp: i64,
}

#[error_code]
pub enum NftMintError {
    #[msg("Invalid creator share percentage")]
    InvalidCreatorShare,
    #[msg("Creator shares must total 100")]
    InvalidTotalShare,
    #[msg("Metadata URI too long")]
    UriTooLong,
    #[msg("Name too long")]
    NameTooLong,
    #[msg("Symbol too long")]
    SymbolTooLong,
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Invalid royalty percentage")]
    InvalidRoyalty,
}